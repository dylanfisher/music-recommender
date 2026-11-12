//! The Discogs API client against a stub server with made-up replies in the API's shape.

#[path = "support/stub.rs"]
mod stub;

use std::net::TcpListener;
use std::time::Duration;

use sources::discogs::api::{Client, Community, ListRow, Outcome, Page, RateLimit};
use stub::{Stub, reply};

const TOKEN: &str = "made-up-token";

/// The values in `stub::RELEASE_JSON`.
const VALUES: Community = Community {
    rating_average: stub::RATING_AVERAGE,
    rating_count: stub::RATING_COUNT,
    have: stub::HAVE,
    want: stub::WANT,
    num_for_sale: Some(stub::NUM_FOR_SALE),
    lowest_price: Some(stub::LOWEST_PRICE),
};

#[test]
fn sends_the_token_and_user_agent_and_reads_the_community_values() {
    let stub = Stub::start(|_| {
        reply(
            200,
            &[
                ("X-Discogs-Ratelimit", "60"),
                ("X-Discogs-Ratelimit-Remaining", "57"),
                ("X-Discogs-Ratelimit-Used", "3"),
            ],
            stub::RELEASE_JSON,
        )
    });
    let reply = Client::new(&stub.url, TOKEN).release(4242);
    assert_eq!(
        reply.outcome,
        Outcome::Found {
            values: VALUES,
            body: stub::RELEASE_JSON.to_string(),
        }
    );
    assert_eq!(
        reply.rate_limit,
        RateLimit {
            limit: Some(60),
            remaining: Some(57),
            used: Some(3),
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
fn the_body_comes_back_byte_for_byte() {
    // An escape, a character outside ASCII and the fixture's own layout, which parsing and
    // writing the JSON again would each change.
    let body = stub::RELEASE_JSON.replacen(
        r#""title": "Made-Up Album""#,
        r#""title": "Made-Up \u00c4lbum Ü""#,
        1,
    );
    assert_ne!(body, stub::RELEASE_JSON);
    assert_eq!(
        outcome_of(reply(200, &[], &body)),
        Outcome::Found {
            values: VALUES,
            body,
        }
    );
}

#[test]
fn a_reply_without_the_price_fields_is_still_values() {
    let mut release: serde_json::Value = serde_json::from_str(stub::RELEASE_JSON).unwrap();
    let fields = release.as_object_mut().unwrap();
    fields.remove("num_for_sale");
    fields.insert("lowest_price".to_string(), serde_json::Value::Null);
    let null_price = release.to_string();
    release.as_object_mut().unwrap().remove("lowest_price");
    let neither = release.to_string();
    for body in [null_price, neither] {
        assert_eq!(
            outcome_of(reply(200, &[], &body)),
            Outcome::Found {
                values: Community {
                    num_for_sale: None,
                    lowest_price: None,
                    ..VALUES
                },
                body,
            }
        );
    }
}

#[test]
fn a_price_field_of_the_wrong_type_is_none_and_the_reply_is_still_kept_whole() {
    use serde_json::json;

    let with = |field: &str, value: serde_json::Value| {
        let mut release: serde_json::Value = serde_json::from_str(stub::RELEASE_JSON).unwrap();
        release[field] = value;
        release.to_string()
    };
    let no_price = Community {
        lowest_price: None,
        ..VALUES
    };
    let no_count = Community {
        num_for_sale: None,
        ..VALUES
    };
    // Each field alone is wrong, so the other keeps its value.
    let cases = [
        (with("lowest_price", json!("12.00")), no_price),
        (with("lowest_price", json!(true)), no_price),
        (with("num_for_sale", json!("3")), no_count),
        (with("num_for_sale", json!(-1)), no_count),
        (with("num_for_sale", json!(2.5)), no_count),
        (with("num_for_sale", json!([])), no_count),
    ];
    for (body, values) in cases {
        assert_eq!(
            outcome_of(reply(200, &[], &body)),
            Outcome::Found { values, body }
        );
    }
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

/// The owner's recorded replies (phase 2's checkpoint), with `stats.user` and contributors'
/// names taken out.
const ARTIST_RELEASES: &str = include_str!("fixtures/discogs-api/artist-releases.json");
const MASTER_VERSIONS: &str = include_str!("fixtures/discogs-api/master-versions.json");

/// The page a list request answered with `body` came to, and the path it asked for.
fn page_of(
    body: &'static str,
    ask: fn(&Client) -> sources::discogs::api::Reply<Page>,
) -> (Page, String) {
    let stub = Stub::start(move |_| reply(200, &[], body));
    let reply = ask(&Client::new(&stub.url, TOKEN));
    let Outcome::Found { values, body: kept } = reply.outcome else {
        panic!("{:?}", reply.outcome);
    };
    assert_eq!(kept, body);
    (values, stub.next_request().path)
}

#[test]
fn reads_an_artist_s_releases_a_page_of_100_with_each_row_s_counts() {
    let (page, path) = page_of(ARTIST_RELEASES, |client| client.artist_releases(72872, 3));
    assert_eq!(path, "/artists/72872/releases?per_page=100&page=3");
    assert_eq!(page.pages, 29);
    assert_eq!(page.rows.len(), 100);
    assert_eq!(page.rows.iter().filter(|row| row.master).count(), 62);
    assert_eq!(
        page.rows[0],
        ListRow {
            master: false,
            id: 3_686_862,
            have: 51,
            want: 27
        }
    );
    assert_eq!(
        page.rows[2],
        ListRow {
            master: true,
            id: 96554,
            have: 463,
            want: 36
        }
    );
}

#[test]
fn reads_a_master_s_versions_with_each_release_s_counts() {
    let (page, path) = page_of(MASTER_VERSIONS, |client| client.master_versions(96559, 1));
    assert_eq!(path, "/masters/96559/versions?per_page=100&page=1");
    assert_eq!(page.pages, 1);
    assert_eq!(page.rows.len(), 67);
    assert_eq!(
        (page.rows[0], page.rows[66]),
        (
            ListRow {
                master: false,
                id: 182_213,
                have: 1396,
                want: 579
            },
            ListRow {
                master: false,
                id: 3_341_754,
                have: 13,
                want: 33
            }
        )
    );
}

#[test]
fn a_list_row_without_its_counts_is_a_problem_and_a_list_shares_the_statuses() {
    let without = r#"{"pagination": {"pages": 1}, "versions": [{"id": 1, "stats": {}}]}"#;
    let stub = Stub::start(move |_| reply(200, &[], without));
    let outcome = Client::new(&stub.url, TOKEN).master_versions(1, 1).outcome;
    assert!(
        matches!(&outcome, Outcome::ReleaseProblem(m) if m.contains("community")),
        "{outcome:?}"
    );
    let stub = Stub::start(|_| reply(404, &[], "{}"));
    let outcome = Client::new(&stub.url, TOKEN).artist_releases(1, 1).outcome;
    assert_eq!(outcome, Outcome::NotFound);
    let stub = Stub::start(|_| reply(429, &[("Retry-After", "7")], ""));
    let outcome = Client::new(&stub.url, TOKEN).artist_releases(1, 1).outcome;
    assert_eq!(outcome, Outcome::RateLimited(Some(Duration::from_secs(7))));
}
