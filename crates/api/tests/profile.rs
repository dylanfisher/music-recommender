//! `POST /v1/profile` over real `TcpStream`s on `127.0.0.1`: the body `profile load` reads,
//! kept and queued for the community sync the same way, and the refusals docs/api.md lists.

#[path = "support/client.rs"]
mod client;
#[path = "../../config/tests/support/setup.rs"]
mod support;

use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use client::{Reply, post, post_with, send};
use features::profile;
use features::status::Community;
use serde_json::{Value, json};

const PATH: &str = "/v1/profile";

/// The profile `profile load` reads in the features tests: three albums, two with ids.
fn fixture() -> String {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../features/tests/fixtures/profile.json");
    fs::read_to_string(path).unwrap()
}

/// A server on a config written for `test`, and its data directory.
fn server(test: &str) -> (SocketAddr, PathBuf) {
    let dir = support::fresh_dir("profile", test);
    let (path, data_dir) = support::write_config(&dir);
    (
        client::serve(config::load(&path).unwrap(), api::LIMITS),
        data_dir,
    )
}

fn profile(addr: SocketAddr, body: &str) -> Reply {
    send(addr, &post(PATH, body))
}

fn bad_request(reply: &Reply) -> &str {
    assert_eq!(reply.status, 400, "{reply:?}");
    assert_eq!(reply.code(), "bad_request");
    reply.body["error"]["message"].as_str().unwrap()
}

#[test]
fn keeps_and_queues_the_profile_as_profile_load_does() {
    let (addr, data_dir) = server("queues");
    let reply = profile(addr, &fixture());
    assert_eq!(reply.status, 200, "{reply:?}");
    assert_eq!(
        reply.body,
        json!({"ok": true, "albums": 3, "with_release_id": 2, "queued": 2})
    );
    let db = Community::open(&data_dir).unwrap();
    assert_eq!(db.next_due(0).unwrap(), Some(4242));
    assert_eq!(db.progress(0).unwrap().library.queued, 2);
    assert_eq!(
        profile::kept(&data_dir).unwrap(),
        Some(profile::parse(fixture().as_bytes()).unwrap())
    );
    // Sent again, nothing is new.
    let again = profile(addr, &fixture());
    assert_eq!(again.body["queued"], 0, "{again:?}");
}

#[test]
fn refuses_a_profile_outside_the_contract_naming_the_field() {
    let (addr, data_dir) = server("refuses");
    let valid = fixture();
    let message = bad_request(&profile(addr, &valid.replace("\"plays\": 31,", ""))).to_string();
    assert!(message.contains("plays"), "{message}");
    let message = bad_request(&profile(
        addr,
        &valid.replace("\"loved\": 1,", "\"lovd\": 1,"),
    ))
    .to_string();
    assert!(message.contains("lovd"), "{message}");
    let message = bad_request(&profile(addr, &valid.replace("4243", "0"))).to_string();
    assert!(message.contains("albums[2]"), "{message}");
    let message = bad_request(&profile(
        addr,
        &valid.replace("\"rating\": 4", "\"rating\": 6"),
    ))
    .to_string();
    assert!(message.contains("albums[0]"), "{message}");
    bad_request(&profile(addr, "[]"));
    bad_request(&profile(addr, &valid[..valid.len() / 2]));
    // A refused profile queues nothing and isn't kept.
    let db = Community::open(&data_dir).unwrap();
    assert_eq!(db.next_due(0).unwrap(), None);
    assert!(profile::kept(&data_dir).unwrap().is_none());
}

/// A profile of `albums` albums, each with its own release id from 1.
fn library(albums: usize) -> String {
    let albums: Vec<Value> = (1..=albums)
        .map(|id| {
            json!({
                "artist": "Made-Up Artist", "title": format!("Made-Up Album {id}"),
                "track_count": 2, "durations": [245, 750], "catalog_numbers": ["MU 001"],
                "barcodes": ["012345678905"], "discogs_release_id": id, "plays": 31,
                "loved": 1, "rating": 4
            })
        })
        .collect();
    json!({"albums": albums}).to_string()
}

#[test]
fn takes_a_profile_larger_than_the_other_routes_take() {
    let (addr, _) = server("large");
    let body = library(2000);
    assert!(body.len() > api::LIMITS.max_body, "{}", body.len());
    let reply = profile(addr, &body);
    assert_eq!(reply.status, 200, "{reply:?}");
    assert_eq!(reply.body["queued"], 2000);
    // The larger cap is the profile's alone.
    let padded = format!("{{{}}}", " ".repeat(body.len()));
    let reply = send(addr, &post("/v1/ping", &padded));
    assert_eq!(
        (reply.status, reply.code()),
        (400, "too_large"),
        "{reply:?}"
    );
}

#[test]
fn refuses_a_profile_over_its_cap_and_a_large_one_without_the_token() {
    let (addr, _) = server("over");
    let token = client::bearer();
    let over = format!(
        "POST {PATH} HTTP/1.1\r\n{token}\r\nContent-Length: {}\r\n\r\n",
        api::LIMITS.max_profile_body + 1
    );
    let reply = send(addr, over.as_bytes());
    assert_eq!(
        (reply.status, reply.code()),
        (400, "too_large"),
        "{reply:?}"
    );
    // Without the token, a body larger than the other routes take isn't read.
    let large = format!(
        "POST {PATH} HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
        api::LIMITS.max_body + 1
    );
    let reply = send(addr, large.as_bytes());
    assert_eq!(
        (reply.status, reply.code()),
        (401, "unauthorized"),
        "{reply:?}"
    );
    let reply = send(
        addr,
        &post_with(PATH, &["Authorization: Bearer wrong"], "{}"),
    );
    assert_eq!(reply.status, 401, "{reply:?}");
}
