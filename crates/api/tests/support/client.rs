//! A client for the API tests, which include this file with `#[path]`: a server on
//! `127.0.0.1` for a config, and raw requests and replies over real `TcpStream`s.

#![allow(
    dead_code,
    reason = "each test crate that includes this file uses part of it"
)]

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;

use api::Limits;
use features::clock::{Clock, SystemClock};
use features::list::SeedSlots;
use features::rescore::Requests;
use serde_json::Value;

use crate::support;

/// Serves the API for `config` with `limits` on a free port, and returns its address.
pub(crate) fn serve(config: config::Config, limits: Limits) -> SocketAddr {
    serve_at(config, limits, Arc::new(SystemClock))
}

/// `serve`, with the status page reading the time from `clock`.
pub(crate) fn serve_at(
    config: config::Config,
    limits: Limits,
    clock: Arc<dyn Clock>,
) -> SocketAddr {
    serve_with(config, limits, clock, Requests::default())
}

/// `serve_at`, with a kept profile asking `rescore` for the scores' rebuild.
pub(crate) fn serve_with(
    config: config::Config,
    limits: Limits,
    clock: Arc<dyn Clock>,
    rescore: Requests,
) -> SocketAddr {
    serve_slots(
        config,
        limits,
        clock,
        rescore,
        Arc::new(SeedSlots::new(api::SEED_REQUESTS)),
    )
}

/// `serve_with`, with `seed_slots` for the requests that compute or wait for a seed list.
pub(crate) fn serve_slots(
    config: config::Config,
    limits: Limits,
    clock: Arc<dyn Clock>,
    rescore: Requests,
    seed_slots: Arc<SeedSlots>,
) -> SocketAddr {
    let config = Arc::new(config);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || api::serve(&listener, &config, &clock, &limits, &rescore, &seed_slots));
    addr
}

/// A reply as read off the wire. Header names are lowercase.
#[derive(Debug)]
pub(crate) struct Reply {
    pub(crate) status: u16,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Value,
}

impl Reply {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// The `error.code` of an `{"ok": false}` body.
    pub(crate) fn code(&self) -> &str {
        assert_eq!(self.body["ok"], false, "{self:?}");
        self.body["error"]["code"].as_str().unwrap()
    }
}

/// A reply with its body as text, for the status page's HTML. Header names are lowercase.
#[derive(Debug)]
pub(crate) struct Page {
    pub(crate) status: u16,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: String,
}

impl Page {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Reads one reply to its end, as the server closes the connection after it.
pub(crate) fn read_reply(stream: TcpStream) -> Reply {
    let page = read_page(stream);
    Reply {
        status: page.status,
        headers: page.headers,
        body: serde_json::from_str(&page.body).unwrap(),
    }
}

/// Reads one reply to its end, its body as text.
pub(crate) fn read_page(mut stream: TcpStream) -> Page {
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8(raw).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .unwrap()
        .split(' ')
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers: Vec<(String, String)> = lines
        .map(|line| {
            let (name, value) = line.split_once(':').unwrap();
            (name.to_lowercase(), value.trim().to_string())
        })
        .collect();
    let length: usize = headers
        .iter()
        .find(|(n, _)| n == "content-length")
        .unwrap()
        .1
        .parse()
        .unwrap();
    assert_eq!(length, body.len(), "{head}");
    Page {
        status,
        headers,
        body: body.to_string(),
    }
}

/// Sends `raw` as it is and reads the reply, its body as text.
pub(crate) fn fetch(addr: SocketAddr, raw: &[u8]) -> Page {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.write_all(raw).unwrap();
    read_page(stream)
}

/// A GET of `path` with `headers`, each a whole line.
pub(crate) fn get_with(path: &str, headers: &[&str]) -> Vec<u8> {
    let mut raw = format!("GET {path} HTTP/1.1\r\nHost: test\r\n");
    for header in headers {
        raw.push_str(header);
        raw.push_str("\r\n");
    }
    raw.push_str("\r\n");
    raw.into_bytes()
}

/// An `Authorization: Basic` header for `user` and `password`, as a browser sends it.
pub(crate) fn basic(user: &str, password: &str) -> String {
    let credentials = format!("{user}:{password}");
    format!("Authorization: Basic {}", base64(credentials.as_bytes()))
}

/// Standard base64 with padding.
pub(crate) fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let group = chunk.iter().enumerate().fold(0u32, |group, (i, &byte)| {
            group | u32::from(byte) << (16 - 8 * i)
        });
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[(group >> (18 - 6 * i) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Sends `raw` as it is and reads the reply.
pub(crate) fn send(addr: SocketAddr, raw: &[u8]) -> Reply {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.write_all(raw).unwrap();
    read_reply(stream)
}

/// A POST to `path` with `headers` (each a whole line) and `body`, `Content-Length` added.
pub(crate) fn post_with(path: &str, headers: &[&str], body: &str) -> Vec<u8> {
    let mut raw = format!("POST {path} HTTP/1.1\r\nHost: test\r\n");
    for header in headers {
        raw.push_str(header);
        raw.push_str("\r\n");
    }
    write!(raw, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    raw.into_bytes()
}

pub(crate) fn bearer() -> String {
    format!("Authorization: Bearer {}", support::PLAYER_TOKEN)
}

/// A POST to `path` with the right token.
pub(crate) fn post(path: &str, body: &str) -> Vec<u8> {
    post_with(path, &[&bearer()], body)
}
