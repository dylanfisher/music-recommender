//! The client for data.discogs.com against a stub server: a year's listing, a download
//! streamed to a writer with its SHA-256, the waits it reports, and the replies it refuses.

#[path = "support/stub.rs"]
mod stub;

use std::time::Duration;

use sources::discogs::download::{Client, Fetched};
use sources::discogs::listing::{self, Size};
use stub::{Stub, reply, reply_bytes};

/// A download's body, not valid UTF-8.
const BODY: &[u8] = b"\x1f\x8b not really gzip, but bytes all the same \x00\xff";

/// The size a listing gives a file of `bytes` bytes.
fn listed(bytes: usize) -> Size {
    let page = format!(
        "2026-11-01 16:40:37             {bytes} B          \
         <a href=\"?download=data%2F2026%2Fdiscogs_20261101_labels.xml.gz\">x</a>"
    );
    listing::parse(&page)[0].size
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |hex, byte| hex + &format!("{byte:02x}"))
}

#[test]
fn reads_a_years_listing() {
    let stub = Stub::start(|_| reply(200, &[], "<pre>listing</pre>"));
    let client = Client::new(&stub.url);
    assert_eq!(
        client.listing(2026),
        Fetched::Done("<pre>listing</pre>".to_string())
    );
    let request = stub.next_request();
    assert_eq!(request.method, "GET");
    assert_eq!(request.path, "/?prefix=data%2F2026%2F");
    assert!(
        request
            .header("user-agent")
            .is_some_and(|agent| agent.starts_with("music-recommender/")),
        "{:?}",
        request.headers
    );
}

#[test]
fn streams_a_download_and_hashes_it() {
    let stub = Stub::start_bytes(|_| reply_bytes(200, &[("ETag", "\"abc\"")], BODY));
    let client = Client::new(&stub.url);
    let mut written = Vec::new();
    let Fetched::Done(downloaded) = client.download(
        "discogs_20261101_labels.xml.gz",
        listed(BODY.len()),
        &mut written,
    ) else {
        panic!("not downloaded");
    };
    assert_eq!(written, BODY);
    assert_eq!(downloaded.written, BODY.len() as u64);
    assert_eq!(downloaded.sha256, sha256(BODY));
    assert_eq!(
        stub.next_request().path,
        "/?download=data%2F2026%2Fdiscogs_20261101_labels.xml.gz"
    );
}

#[test]
fn a_429_or_a_503_with_retry_after_is_a_wait() {
    let stub = Stub::start(|request| {
        if request.path.contains("2025") {
            reply(503, &[("Retry-After", "5")], "")
        } else {
            reply(429, &[("Retry-After", "2750")], "")
        }
    });
    let client = Client::new(&stub.url);
    assert_eq!(
        client.listing(2026),
        Fetched::Wait(Some(Duration::from_secs(2750)))
    );
    assert_eq!(
        client.listing(2025),
        Fetched::Wait(Some(Duration::from_secs(5)))
    );
    let stub = Stub::start(|_| reply(429, &[], ""));
    assert_eq!(Client::new(&stub.url).listing(2026), Fetched::Wait(None));
}

#[test]
fn refuses_other_replies_naming_the_status() {
    for (status, headers) in [
        (500, vec![]),
        (503, vec![]),
        (302, vec![("Location", "/elsewhere")]),
    ] {
        let stub = Stub::start(move |_| reply(status, &headers, ""));
        let mut written = Vec::new();
        match Client::new(&stub.url).download(
            "discogs_20261101_labels.xml.gz",
            listed(BODY.len()),
            &mut written,
        ) {
            Fetched::Failed(message) => {
                assert!(message.contains(&status.to_string()), "{message}");
            }
            other => panic!("{status}: {other:?}"),
        }
        assert_eq!(written, b"");
    }
}

#[test]
fn a_reply_cut_short_fails() {
    let stub = Stub::start_bytes(|_| {
        let mut raw =
            b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nConnection: close\r\n\r\n".to_vec();
        raw.extend_from_slice(BODY);
        raw
    });
    let mut written = Vec::new();
    let outcome = Client::new(&stub.url).download(
        "discogs_20261101_labels.xml.gz",
        listed(1000),
        &mut written,
    );
    assert!(matches!(outcome, Fetched::Failed(_)), "{outcome:?}");
}

#[test]
fn no_server_fails() {
    // A port just freed, so nothing listens on it.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let client = Client::new(&format!("http://127.0.0.1:{port}"));
    assert!(matches!(client.listing(2026), Fetched::Failed(_)));
}

#[test]
fn a_retry_after_given_as_a_date_is_a_wait_of_no_length() {
    const DATE: &str = "Wed, 11 Nov 2026 05:00:00 GMT";
    for status in [429, 503] {
        let stub = Stub::start(move |_| reply(status, &[("Retry-After", DATE)], ""));
        assert_eq!(
            Client::new(&stub.url).listing(2026),
            Fetched::Wait(None),
            "{status}"
        );
    }
}

#[test]
fn a_404_is_its_own_answer() {
    let stub = Stub::start(|_| reply(404, &[], "Not Found"));
    let client = Client::new(&stub.url);
    assert_eq!(client.listing(2027), Fetched::Missing);
    let mut written = Vec::new();
    assert_eq!(
        client.download("discogs_20261101_labels.xml.gz", listed(1), &mut written),
        Fetched::Missing
    );
}

#[test]
fn a_content_length_not_the_listings_size_fails_before_a_byte_is_written() {
    let stub = Stub::start_bytes(|_| reply_bytes(200, &[], BODY));
    let mut written = Vec::new();
    let outcome = Client::new(&stub.url).download(
        "discogs_20261101_labels.xml.gz",
        listed(BODY.len() + 1),
        &mut written,
    );
    assert!(
        matches!(&outcome, Fetched::Failed(m) if m.contains("listing")),
        "{outcome:?}"
    );
    assert_eq!(written, b"");
}

#[test]
fn a_reply_without_a_content_length_fails_before_a_byte_is_written() {
    let stub = Stub::start_bytes(|_| {
        let mut raw = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
        raw.extend_from_slice(BODY);
        raw
    });
    let mut written = Vec::new();
    let outcome = Client::new(&stub.url).download(
        "discogs_20261101_labels.xml.gz",
        listed(BODY.len()),
        &mut written,
    );
    assert!(
        matches!(&outcome, Fetched::Failed(m) if m.contains("Content-Length")),
        "{outcome:?}"
    );
    assert_eq!(written, b"");
}
