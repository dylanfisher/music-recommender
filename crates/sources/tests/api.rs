//! The Discogs API client against a stub server with made-up replies in the API's shape.

#[path = "support/stub.rs"]
mod stub;

use std::net::TcpListener;
use std::time::Duration;

use sources::discogs::api::{Client, Community, Outcome, RateLimit};
use stub::{Stub, reply};

const TOKEN: &str = "made-up-token";

#[test]
fn sends_the_token_and_user_agent_and_reads_the_community_values() {
    let stub = Stub::start(|_| {
        reply(
            200,
            &[
                ("X-Discogs-Ratelimit", "60"),
                ("X-Discogs-Ratelimit-Remaining", "57"),
            ],
            stub::RELEASE_JSON,
        )
    });
    let reply = Client::new(&stub.url, TOKEN).release(4242);
    assert_eq!(
        reply.outcome,
        Outcome::Found(Community {
            rating_average: stub::RATING_AVERAGE,
            rating_count: stub::RATING_COUNT,
            have: stub::HAVE,
            want: stub::WANT,
        })
    );
    assert_eq!(
        reply.rate_limit,
        RateLimit {
            limit: Some(60),
            remaining: Some(57),
        }
    );
    let request = stub.next_request();
    assert_eq!(request.method, "GET");
    assert_eq!(request.path, "/releases/4242");
    assert_eq!(request.body, b"");
    assert_eq!(
        request.header("authorization"),
        Some("Discogs token=made-up-token")
    );
    assert_eq!(
        request.header("user-agent"),
        Some(concat!("music-recommender/", env!("CARGO_PKG_VERSION")))
    );
}

/// The outcome of one request answered with `response`.
fn outcome_of(response: String) -> Outcome {
    let stub = Stub::start(move |_| response.clone());
    Client::new(&stub.url, TOKEN).release(1).outcome
}

#[test]
fn types_each_status() {
    assert_eq!(outcome_of(reply(404, &[], "{}")), Outcome::NotFound);
    assert_eq!(outcome_of(reply(401, &[], "{}")), Outcome::Refused(401));
    assert_eq!(outcome_of(reply(403, &[], "{}")), Outcome::Refused(403));
    assert_eq!(
        outcome_of(reply(502, &[], "")),
        Outcome::Trouble("Discogs answered 502".to_string())
    );
    assert_eq!(
        outcome_of(reply(429, &[("Retry-After", "17")], "{}")),
        Outcome::RateLimited(Some(Duration::from_secs(17)))
    );
    assert_eq!(
        outcome_of(reply(429, &[], "{}")),
        Outcome::RateLimited(None)
    );
    // Discogs can't answer now, whatever the release.
    for status in [408, 425] {
        assert_eq!(
            outcome_of(reply(status, &[], "")),
            Outcome::Trouble(format!("Discogs answered {status}"))
        );
    }
    // Any other status is a problem with the release that names it.
    for status in [400, 410, 418, 204] {
        let outcome = outcome_of(reply(status, &[], ""));
        assert!(
            matches!(&outcome, Outcome::ReleaseProblem(m) if m.contains(&status.to_string())),
            "{status}: {outcome:?}"
        );
    }
    let redirect = outcome_of(reply(302, &[("Location", "/elsewhere")], ""));
    assert!(
        matches!(&redirect, Outcome::ReleaseProblem(m) if m.contains("302")),
        "{redirect:?}"
    );
}

#[test]
fn a_reply_without_the_community_values_is_a_release_problem() {
    let outcome = outcome_of(reply(200, &[], r#"{"id": 1, "title": "x"}"#));
    assert!(
        matches!(&outcome, Outcome::ReleaseProblem(m) if m.contains("community")),
        "{outcome:?}"
    );
    assert!(matches!(
        outcome_of(reply(200, &[], "<html>")),
        Outcome::ReleaseProblem(_)
    ));
}

#[test]
fn a_reply_over_one_megabyte_is_refused() {
    let padding = " ".repeat(1024 * 1024);
    let body = format!("{}{padding}", stub::RELEASE_JSON);
    let outcome = outcome_of(reply(200, &[], &body));
    assert!(
        matches!(&outcome, Outcome::ReleaseProblem(m) if m.contains("bytes")),
        "{outcome:?}"
    );
}

#[test]
fn a_refused_connection_is_a_network_error() {
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let reply = Client::new(&format!("http://127.0.0.1:{port}"), TOKEN).release(1);
    assert!(
        matches!(&reply.outcome, Outcome::Trouble(m) if m.starts_with("network:")),
        "{:?}",
        reply.outcome
    );
    assert_eq!(reply.rate_limit, RateLimit::default());
}
