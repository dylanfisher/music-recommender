//! The HTTP server over real `TcpStream`s on `127.0.0.1`: each refusal, the token, the
//! connection cap and the read deadline. Nothing sleeps: a test waits for the server's reply
//! or for the connection to close.

#[path = "support/client.rs"]
mod client;
#[path = "../../config/tests/support/setup.rs"]
mod support;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

use api::Limits;
use client::{bearer, post, post_with, read_reply, send};

/// How long a test waits for something it expects, such as a free connection slot. A
/// condition, not a delay: only a broken test runs into it.
const EXPECTED_WITHIN: Duration = Duration::from_secs(30);

/// Starts a server with `limits` on a config written for `test`, and returns its address.
fn start(test: &str, limits: Limits) -> SocketAddr {
    let dir = support::fresh_dir("server", test);
    let (path, _) = support::write_config(&dir);
    client::serve(config::load(&path).unwrap(), limits)
}

fn server(test: &str) -> SocketAddr {
    start(test, api::LIMITS)
}

#[test]
fn ping_answers_with_the_version() {
    let addr = server("ping");
    let reply = send(addr, &post("/v1/ping", "{}"));
    assert_eq!(reply.status, 200, "{reply:?}");
    assert_eq!(reply.body["ok"], true);
    assert_eq!(reply.body["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(reply.header("connection"), Some("close"));
    assert_eq!(reply.header("content-type"), Some("application/json"));
}

#[test]
fn refuses_a_missing_or_wrong_token() {
    let addr = server("token");
    let token = support::PLAYER_TOKEN;
    let short = &token[..token.len() - 1];
    let wrong = [
        String::new(),
        "Authorization: Bearer".to_string(),
        "Authorization: Bearer wrong".to_string(),
        format!("Authorization: Bearer {short}"),
        format!("Authorization: Bearer {token}x"),
        format!("Authorization: Basic {token}"),
        format!("Authorization: {token}"),
    ];
    for header in wrong {
        let headers: Vec<&str> = [header.as_str()]
            .into_iter()
            .filter(|h| !h.is_empty())
            .collect();
        let reply = send(addr, &post_with("/v1/ping", &headers, "{}"));
        assert_eq!(reply.status, 401, "{header}: {reply:?}");
        assert_eq!(reply.code(), "unauthorized", "{header}");
        assert_eq!(reply.header("www-authenticate"), Some("Bearer"));
        assert!(!reply.body.to_string().contains(token), "{reply:?}");
    }
}

#[test]
fn refuses_a_method_other_than_post() {
    let addr = server("method");
    let raw = format!(
        "GET /v1/ping HTTP/1.1\r\nHost: test\r\n{}\r\n\r\n",
        bearer()
    );
    let reply = send(addr, raw.as_bytes());
    assert_eq!(reply.status, 405, "{reply:?}");
    assert_eq!(reply.code(), "method_not_allowed");
    assert_eq!(reply.header("allow"), Some("POST"));
}

#[test]
fn answers_404_for_an_unknown_route() {
    let addr = server("unknown_route");
    for path in ["/v1/pong", "/v1/ping/", "/v1/ping?x=1", "/"] {
        let reply = send(addr, &post(path, "{}"));
        assert_eq!(reply.status, 404, "{path}: {reply:?}");
        assert_eq!(reply.code(), "not_found", "{path}");
    }
}

#[test]
fn checks_the_token_before_the_route() {
    // An unknown route without the token is a 401, so routes can't be probed without it.
    let addr = server("token_first");
    let reply = send(addr, &post_with("/v1/pong", &[], "{}"));
    assert_eq!(reply.status, 401, "{reply:?}");
}

#[test]
fn refuses_malformed_requests() {
    let addr = server("malformed");
    let token = bearer();
    let cases: [(&str, Vec<u8>); 9] = [
        ("garbage", b"hello\r\n\r\n".to_vec()),
        ("http 2", b"POST /v1/ping HTTP/2\r\n\r\n".to_vec()),
        (
            "no length",
            format!("POST /v1/ping HTTP/1.1\r\n{token}\r\n\r\n{{}}").into_bytes(),
        ),
        (
            "bad length",
            format!("POST /v1/ping HTTP/1.1\r\n{token}\r\nContent-Length: two\r\n\r\n{{}}")
                .into_bytes(),
        ),
        (
            "two lengths",
            post_with("/v1/ping", &[&token, "Content-Length: 2"], "{}"),
        ),
        (
            "chunked",
            post_with("/v1/ping", &[&token, "Transfer-Encoding: chunked"], "{}"),
        ),
        ("not json", post("/v1/ping", "{oops")),
        ("not an object", post("/v1/ping", "[]")),
        ("unknown field", post("/v1/ping", r#"{"extra": 1}"#)),
    ];
    for (name, raw) in cases {
        let reply = send(addr, &raw);
        assert_eq!(reply.status, 400, "{name}: {reply:?}");
        assert_eq!(reply.code(), "bad_request", "{name}");
    }
}

#[test]
fn refuses_oversize_requests() {
    let addr = server("oversize");
    let token = bearer();
    let long_header = format!("X-Long: {}", "a".repeat(9 * 1024));
    let many: Vec<String> = (0..70).map(|i| format!("X-Header-{i}: {i}")).collect();
    let mut many_headers: Vec<&str> = many.iter().map(String::as_str).collect();
    many_headers.push(&token);
    let cases: [(&str, Vec<u8>); 4] = [
        (
            "body",
            format!(
                "POST /v1/ping HTTP/1.1\r\n{token}\r\nContent-Length: {}\r\n\r\n",
                api::LIMITS.max_body + 1
            )
            .into_bytes(),
        ),
        (
            "request line",
            format!("POST /{} HTTP/1.1\r\n\r\n", "a".repeat(9 * 1024)).into_bytes(),
        ),
        (
            "header",
            post_with("/v1/ping", &[&token, &long_header], "{}"),
        ),
        ("headers", post_with("/v1/ping", &many_headers, "{}")),
    ];
    for (name, raw) in cases {
        let reply = send(addr, &raw);
        assert_eq!(reply.status, 400, "{name}: {reply:?}");
        assert_eq!(reply.code(), "too_large", "{name}");
    }
}

#[test]
fn accepts_a_body_at_the_cap() {
    // JSON allows whitespace, so a `{}` padded to exactly the cap is a valid ping.
    let addr = server("at_cap");
    let body = format!("{{{}}}", " ".repeat(api::LIMITS.max_body - 2));
    let reply = send(addr, &post("/v1/ping", &body));
    assert_eq!(reply.status, 200, "{reply:?}");
}

#[test]
fn cuts_off_a_client_that_stops_sending() {
    let limits = Limits {
        read_deadline: Duration::from_millis(200),
        ..api::LIMITS
    };
    let addr = start("slow", limits);
    let token = bearer();
    let partial = [
        "POST /v1/ping HTTP/1.1\r\n".to_string(),
        format!("POST /v1/ping HTTP/1.1\r\n{token}\r\nContent-Length: 2\r\n\r\n{{"),
    ];
    for raw in partial {
        let started = Instant::now();
        let reply = send(addr, raw.as_bytes());
        assert_eq!(reply.status, 408, "{raw:?}: {reply:?}");
        assert_eq!(reply.code(), "timeout");
        assert!(started.elapsed() < EXPECTED_WITHIN);
    }
}

#[test]
fn caps_the_connections_at_once() {
    let limits = Limits {
        connections: 2,
        ..api::LIMITS
    };
    let addr = start("cap", limits);
    // Two idle connections take both slots. The server accepts in order, so the third is
    // seen after them and refused. It sends nothing, so the refusal is read in full rather
    // than cut short by a reset over an unread request.
    let first = TcpStream::connect(addr).unwrap();
    let _second = TcpStream::connect(addr).unwrap();
    let reply = read_reply(TcpStream::connect(addr).unwrap());
    assert_eq!(reply.status, 503, "{reply:?}");
    assert_eq!(reply.code(), "busy");

    // Closing one frees its slot once its thread sees the close.
    drop(first);
    let deadline = Instant::now() + EXPECTED_WITHIN;
    loop {
        let mut stream = TcpStream::connect(addr).unwrap();
        // A refusal may reset the connection before the request is written or read.
        let mut raw = Vec::new();
        let answered = stream
            .write_all(&post("/v1/ping", "{}"))
            .and_then(|()| stream.read_to_end(&mut raw));
        if answered.is_ok() && raw.starts_with(b"HTTP/1.1 200 ") {
            break;
        }
        assert!(Instant::now() < deadline, "the slot was never freed");
        thread::yield_now();
    }
}
