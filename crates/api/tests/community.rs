//! `POST /v1/community` over real `TcpStream`s on `127.0.0.1`, against a community database
//! written through the store: each release id's values, or why it has none, as docs/api.md
//! writes them.

#[path = "support/client.rs"]
mod client;
#[path = "../../config/tests/support/setup.rs"]
mod support;

use std::net::SocketAddr;
use std::path::PathBuf;

use client::{Reply, post, send};
use features::status::{Community, Source, Values};
use serde_json::{Value, json};

const VALUES: Values = Values {
    rating_average: 4.25,
    rating_count: 87,
    have: 1203,
    want: 340,
    num_for_sale: Some(12),
    lowest_price: None,
};

/// A server on a config written for `test`, and its data directory.
fn server(test: &str) -> (SocketAddr, PathBuf) {
    let dir = support::fresh_dir("community", test);
    let (path, data_dir) = support::write_config(&dir);
    (
        client::serve(config::load(&path).unwrap(), api::LIMITS),
        data_dir,
    )
}

fn community(addr: SocketAddr, body: &Value) -> Reply {
    send(addr, &post("/v1/community", &body.to_string()))
}

fn ok(addr: SocketAddr, body: &Value) -> Value {
    let reply = community(addr, body);
    assert_eq!(
        (reply.status, &reply.body["ok"]),
        (200, &json!(true)),
        "{reply:?}"
    );
    reply.body
}

fn bad_request(addr: SocketAddr, body: &Value) {
    let reply = community(addr, body);
    assert_eq!((reply.status, reply.code()), (400, "bad_request"), "{body}");
}

#[test]
fn answers_each_release_with_its_values_or_why_it_has_none() {
    let (addr, data_dir) = server("each");
    let mut db = Community::open(&data_dir).unwrap();
    db.queue(&[1, 2, 3, 4, 5], Source::Library).unwrap();
    db.record(1, VALUES, "{}", 1_794_060_000).unwrap();
    db.record_gone(2, 1_794_060_100).unwrap();
    db.record(3, VALUES, "{}", 1_794_060_200).unwrap();
    for attempt in 0..10 {
        db.record_failure(3, "a release problem", 1_794_070_000 + attempt)
            .unwrap();
        db.record_failure(4, "a release problem", 1_794_080_000 + attempt)
            .unwrap();
    }
    let values = json!({"rating_average": 4.25, "rating_count": 87, "have": 1203,
                        "want": 340, "num_for_sale": 12, "lowest_price": null});
    // A repeated id is answered once, where it first came.
    let body = ok(addr, &json!({"release_ids": [1, 2, 3, 4, 5, 6, 1]}));
    assert_eq!(
        body,
        json!({"ok": true, "releases": [
            {"release_id": 1, "status": "fetched", "checked_at": 1_794_060_000, "values": values},
            {"release_id": 2, "status": "gone", "checked_at": 1_794_060_100, "values": null},
            {"release_id": 3, "status": "failed", "checked_at": 1_794_070_009, "values": values},
            {"release_id": 4, "status": "failed", "checked_at": 1_794_080_009, "values": null},
            {"release_id": 5, "status": "queued", "checked_at": null, "values": null},
            {"release_id": 6, "status": "not_queued", "checked_at": null, "values": null},
        ]})
    );
    assert_eq!(ok(addr, &json!({"release_ids": []}))["releases"], json!([]));
}

#[test]
fn reads_no_database_yet_as_nothing_queued_and_creates_none() {
    let (addr, data_dir) = server("missing");
    let body = ok(addr, &json!({"release_ids": [4242]}));
    assert_eq!(body["releases"][0]["status"], "not_queued", "{body}");
    assert_eq!(support::files(&data_dir), Vec::<String>::new());
}

#[test]
fn refuses_a_list_outside_the_contract() {
    let (addr, _) = server("refuses");
    let most: Vec<u64> = (1..=1000).collect();
    let body = ok(addr, &json!({"release_ids": most}));
    assert_eq!(body["releases"].as_array().unwrap().len(), 1000);
    let over: Vec<u64> = (1..=1001).collect();
    for body in [
        json!({"release_ids": over}),
        json!({}),
        json!({"release_ids": [1], "extra": true}),
        json!({"release_ids": 1}),
        json!({"release_ids": [0]}),
        json!({"release_ids": [-1]}),
        json!({"release_ids": ["1"]}),
        json!({"release_ids": [1.5]}),
        json!({"release_ids": [9_223_372_036_854_775_808_u64]}),
    ] {
        bad_request(addr, &body);
    }
}

#[test]
fn a_database_of_another_shape_is_a_500_and_is_left_alone() {
    let (addr, data_dir) = server("other-shape");
    let file = data_dir.join("community.sqlite");
    let old = include_bytes!("../../features/tests/fixtures/previous-schema.sqlite");
    std::fs::write(&file, old).unwrap();
    let before = support::files(&data_dir);
    let reply = community(addr, &json!({"release_ids": [1]}));
    assert_eq!((reply.status, reply.code()), (500, "internal"), "{reply:?}");
    let message = reply.body["error"]["message"].as_str().unwrap();
    assert!(message.contains("another version"), "{message}");
    assert_eq!(std::fs::read(&file).unwrap(), old);
    assert_eq!(support::files(&data_dir), before);
}
