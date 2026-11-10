//! A stub HTTP server on `127.0.0.1`, built on `std::net::TcpListener` (ADR 0005), standing
//! in for the Discogs API and slskd so no test reaches either. It answers each connection
//! with what the test's handler returns, closes it, and reports each request, with its
//! method and its `Content-Length` body, on a channel.
//!
//! The one copy: the features and app tests include this file with `#[path]`.

#![allow(
    dead_code,
    reason = "each test crate that includes this file uses part of it"
)]

use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

/// A made-up release reply in the API's shape, with these community values.
pub(crate) const RELEASE_JSON: &str = include_str!("../fixtures/release.json");
pub(crate) const HAVE: u64 = 1234;
pub(crate) const WANT: u64 = 567;
pub(crate) const RATING_COUNT: u64 = 89;
pub(crate) const RATING_AVERAGE: f64 = 4.21;
pub(crate) const NUM_FOR_SALE: u64 = 12;
pub(crate) const LOWEST_PRICE: f64 = 9.5;

/// How long a test waits for a request it expects. A condition, not a delay: it returns
/// as soon as the request arrives, and only a broken test runs into it.
const EXPECTED_WITHIN: Duration = Duration::from_secs(30);

/// A request as the stub read it. Header names are lowercase.
#[derive(Debug)]
pub(crate) struct Request {
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
}

impl Request {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// The release id of a `/releases/{id}` request.
    pub(crate) fn release_id(&self) -> u64 {
        self.path
            .strip_prefix("/releases/")
            .and_then(|id| id.parse().ok())
            .unwrap_or_else(|| panic!("not a release request: {}", self.path))
    }
}

pub(crate) struct Stub {
    pub(crate) url: String,
    requests: Receiver<Request>,
}

impl Stub {
    /// Serves until the test process ends, answering each request with `handler`'s raw
    /// HTTP response (see [`reply`]).
    pub(crate) fn start(handler: impl Fn(&Request) -> String + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (sender, requests) = mpsc::channel();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let mut parts = line.split(' ');
                let method = parts.next().unwrap_or_default().to_string();
                let path = parts.next().unwrap_or_default().to_string();
                let mut headers = Vec::new();
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    let Some((name, value)) = line.trim_end().split_once(':') else {
                        break;
                    };
                    headers.push((name.to_lowercase(), value.trim().to_string()));
                }
                let length = headers
                    .iter()
                    .find(|(name, _)| name == "content-length")
                    .map_or(0, |(_, value)| value.parse().unwrap());
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let request = Request {
                    method,
                    path,
                    headers,
                    body,
                };
                let response = handler(&request);
                // Reported after the handler, so a test that sees request N knows the
                // handler's side effects for it have happened.
                let _ = sender.send(request);
                // The client may have given up already (a test of the size cap).
                let _ = stream.write_all(response.as_bytes());
            }
        });
        Self { url, requests }
    }

    /// Serves `RELEASE_JSON` for every request.
    pub(crate) fn releases() -> Self {
        Self::start(|_| reply(200, &[], RELEASE_JSON))
    }

    /// The next request, waiting for it to arrive.
    pub(crate) fn next_request(&self) -> Request {
        self.requests
            .recv_timeout(EXPECTED_WITHIN)
            .expect("the stub got no request")
    }

    /// Every request received so far, without waiting.
    pub(crate) fn received(&self) -> Vec<Request> {
        self.requests.try_iter().collect()
    }
}

/// A raw HTTP/1.1 response that closes the connection.
pub(crate) fn reply(status: u16, headers: &[(&str, &str)], body: &str) -> String {
    let mut response = format!(
        "HTTP/1.1 {status} Stub\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in headers {
        write!(response, "{name}: {value}\r\n").unwrap();
    }
    response.push_str("\r\n");
    response.push_str(body);
    response
}
