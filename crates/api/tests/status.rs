//! `GET /status` over real `TcpStream`s on `127.0.0.1`: the Basic sign-in, the page against
//! the status read from the same fixture database, the escaping, and which requests take
//! the page's path and which keep the bearer and POST.

#[path = "support/client.rs"]
mod client;
#[path = "../../features/tests/support/clock.rs"]
mod clock;
#[path = "../../config/tests/support/setup.rs"]
mod support;

use std::net::SocketAddr;
use std::sync::Arc;

use client::{basic, bearer, fetch, get_with, post, post_with, send};
use clock::{FakeClock, START};
use config::Config;
use features::status::{self, Community, Source, Values};

/// What a browser is asked to sign in with.
const SIGN_IN: &str = r#"Basic realm="music-recommender", charset="UTF-8""#;

const HTML: &str = "text/html; charset=utf-8";

/// A last error holding every character the page has to escape.
const HOSTILE: &str = r#"<script>alert(1)</script> "double" 'single' & more"#;

const VALUES: Values = Values {
    rating_average: 3.5,
    rating_count: 4,
    have: 10,
    want: 2,
    num_for_sale: None,
    lowest_price: None,
};

/// A server on a config written for `test`, its clock at `START`, and the config.
fn start(test: &str) -> (SocketAddr, Config) {
    let dir = support::fresh_dir("status", test);
    let (path, _) = support::write_config(&dir);
    let addr = client::serve_at(
        config::load(&path).unwrap(),
        api::LIMITS,
        Arc::new(FakeClock::new()),
    );
    (addr, config::load(&path).unwrap())
}

fn signed_in() -> String {
    basic("owner", support::PLAYER_TOKEN)
}

/// The library's releases 1 to 3 (fetched, gone, never fetched) and the candidates' 10 and
/// 11 (fetched 91 days ago and so due, never fetched), release 3 failed with a hostile error,
/// and a hostile last error.
fn fixture(config: &Config) {
    let mut db = Community::open(&config.data_dir).unwrap();
    db.queue(&[1, 2, 3, 4], Source::Library).unwrap();
    db.queue(&[10, 11], Source::Candidate).unwrap();
    db.record(1, VALUES, "{}", START - 600).unwrap();
    db.record_gone(2, START - 600).unwrap();
    for attempt in 0..10 {
        db.record_failure(3, HOSTILE, START - 300 + attempt)
            .unwrap();
    }
    db.record(10, VALUES, "{}", START - 91 * 24 * 3600).unwrap();
    db.record_error(HOSTILE, START - 120).unwrap();
}

#[test]
fn asks_a_browser_without_the_password_to_sign_in() {
    let (addr, _) = start("sign-in");
    let token = support::PLAYER_TOKEN;
    let short = &token[..token.len() - 1];
    let wrong = [
        String::new(),
        basic("owner", "wrong"),
        basic("owner", short),
        basic("owner", &format!("{token}x")),
        basic("", ""),
        // The token as the user name, with no password.
        basic(token, ""),
        // No colon between a user name and a password.
        format!("Authorization: Basic {}", client::base64(token.as_bytes())),
        "Authorization: Basic".to_string(),
        "Authorization: Basic !!!!".to_string(),
        "Authorization: Basic b3duZXI6".to_string() + "=",
        bearer(),
        format!("Authorization: {token}"),
    ];
    for header in wrong {
        let headers: Vec<&str> = [header.as_str()]
            .into_iter()
            .filter(|h| !h.is_empty())
            .collect();
        let page = fetch(addr, &get_with("/status", &headers));
        assert_eq!(page.status, 401, "{header}: {page:?}");
        assert_eq!(page.header("www-authenticate"), Some(SIGN_IN), "{header}");
        assert_eq!(page.header("content-type"), Some(HTML), "{header}");
        assert_eq!(page.header("connection"), Some("close"), "{header}");
        assert!(!page.body.contains(token), "{page:?}");
    }
}

#[test]
fn signs_in_with_the_token_as_the_password_and_any_user_name() {
    let (addr, _) = start("signed-in");
    for user in ["owner", "", "someone else"] {
        let page = fetch(
            addr,
            &get_with("/status", &[&basic(user, support::PLAYER_TOKEN)]),
        );
        assert_eq!(page.status, 200, "{user}: {page:?}");
        assert_eq!(page.header("content-type"), Some(HTML));
        assert_eq!(page.header("connection"), Some("close"));
        assert!(
            page.body
                .contains(r#"<meta http-equiv="refresh" content="30">"#),
            "{page:?}"
        );
        assert!(page.body.starts_with("<!doctype html>"), "{page:?}");
        assert!(!page.body.contains(support::PLAYER_TOKEN), "{page:?}");
    }
}

#[test]
fn escapes_text_from_the_database() {
    let (addr, config) = start("escaped");
    fixture(&config);
    let page = fetch(addr, &get_with("/status", &[&signed_in()]));
    assert_eq!(page.status, 200, "{page:?}");
    let escaped = "&lt;script&gt;alert(1)&lt;/script&gt; &quot;double&quot; &#39;single&#39; \
                   &amp; more";
    // Once as the failed release's error and once as the last error.
    assert_eq!(page.body.matches(escaped).count(), 2, "{}", page.body);
    assert!(!page.body.contains("<script>"), "{}", page.body);
    assert!(!page.body.contains("'single'"), "{}", page.body);
}

#[test]
fn shows_the_counts_the_status_reads() {
    let (addr, config) = start("counts");
    fixture(&config);
    let page = fetch(addr, &get_with("/status", &[&signed_in()]));
    assert_eq!(page.status, 200, "{page:?}");
    let status = status::read(&config, &FakeClock::new()).unwrap();
    let progress = &status.sync.progress;
    for (name, counts) in [
        ("Library", progress.library),
        ("Candidates", progress.candidates),
    ] {
        let row = format!(
            "<tr><th>{name}</th><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            counts.queued, counts.with_values, counts.failed, counts.gone, counts.due
        );
        assert!(page.body.contains(&row), "{row}\n{}", page.body);
    }
    // The fixture's counts, so the comparison above isn't of two empty tables.
    assert!(
        page.body.contains(
            "<tr><th>Library</th><td>1</td><td>1</td><td>1</td><td>1</td><td>0</td></tr>"
        ),
        "{}",
        page.body
    );
    for line in [
        "<dt>Paused</dt><dd>no</dd>".to_string(),
        "<dt>Running</dt><dd>no</dd>".to_string(),
        format!(
            "<dt>Requests in the last hour</dt><dd>{}</dd>",
            status.sync.requests
        ),
        "<dt>Time left</dt><dd>at least 3 s at 60 a minute</dd>".to_string(),
    ] {
        assert!(page.body.contains(&line), "{line}\n{}", page.body);
    }
}

#[test]
fn shows_the_yield_while_a_sync_holds_the_lock() {
    let (addr, config) = start("yield");
    let db = Community::open(&config.data_dir).unwrap();
    db.set_yielding(Some(START - 300)).unwrap();
    let page = fetch(addr, &get_with("/status", &[&signed_in()]));
    assert!(!page.body.contains("Yielding"), "{}", page.body);
    let _lock = Community::lock(&config.data_dir).unwrap();
    let page = fetch(addr, &get_with("/status", &[&signed_in()]));
    assert!(
        page.body.contains("<dt>Running</dt><dd>yes</dd>")
            && page
                .body
                .contains("<dt>Yielding to another client</dt><dd>for 5 min</dd>"),
        "{}",
        page.body
    );
}

#[test]
fn every_other_request_keeps_post_and_the_bearer() {
    let (addr, _) = start("others");
    // POST to the page's path is a POST route like any other: the bearer first, then no
    // route.
    let reply = send(addr, &post_with("/status", &[], "{}"));
    assert_eq!(reply.status, 401, "{reply:?}");
    assert_eq!(reply.header("www-authenticate"), Some("Bearer"));
    assert_eq!(send(addr, &post("/status", "{}")).status, 404);
    let reply = send(addr, &post_with("/v1/ping", &[&signed_in()], "{}"));
    assert_eq!(reply.status, 401, "{reply:?}");
    // A GET of any other path, a query string included, is refused before any sign-in.
    for (path, header) in [
        ("/v1/ping", bearer()),
        ("/v1/ping", signed_in()),
        ("/status?x=1", signed_in()),
        ("/status/", signed_in()),
        ("/", signed_in()),
    ] {
        let reply = send(addr, &get_with(path, &[&header]));
        assert_eq!(reply.status, 405, "{path}: {reply:?}");
        assert_eq!(reply.code(), "method_not_allowed");
        assert_eq!(reply.header("allow"), Some("POST"));
    }
    let head = b"HEAD /status HTTP/1.1\r\nHost: test\r\n\r\n";
    assert_eq!(send(addr, head).status, 405);
    // A GET has no body.
    let with_body = get_with("/status", &[&signed_in(), "Content-Length: 2"]);
    let reply = send(addr, &[with_body, b"{}".to_vec()].concat());
    assert_eq!(reply.status, 400, "{reply:?}");
    assert_eq!(reply.code(), "bad_request");
}
