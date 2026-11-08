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
use serde_json::Value;

use crate::support;

/// Serves the API for `config` with `limits` on a free port, and returns its address.
pub(crate) fn serve(config: config::Config, limits: Limits) -> SocketAddr {
    let config = Arc::new(config);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || api::serve(&listener, &config, &limits));
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

/// Reads one reply to its end, as the server closes the connection after it.
pub(crate) fn read_reply(mut stream: TcpStream) -> Reply {
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
    Reply {
        status,
        headers,
        body: serde_json::from_str(body).unwrap(),
    }
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
