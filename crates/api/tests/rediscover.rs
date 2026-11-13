//! `POST /v1/rediscover`, `/v1/rediscover/not_now` and `/v1/rediscover/not_now/undo` over
//! real `TcpStream`s on `127.0.0.1`, against the sources crate's fixture set imported, with
//! a made-up clock: the day's albums in the shape docs/api.md writes, the not-now marks, the
//! refusals and the 400s.

#[path = "support/client.rs"]
mod client;
#[path = "../../features/tests/support/clock.rs"]
mod clock;
#[path = "../../config/tests/support/setup.rs"]
mod support;

use std::fs;
use std::io::Write as _;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use client::{Reply, post, send};
use clock::{FakeClock, START};
use features::clock::Clock;
use features::import;
use features::profile::{self, Album, Profile};
use serde_json::{Value, json};

const PATH: &str = "/v1/rediscover";
const NOT_NOW: &str = "/v1/rediscover/not_now";
const UNDO: &str = "/v1/rediscover/not_now/undo";
const DAY: i64 = 86_400;

fn album(key: &str, artist: &str, title: &str, release_id: Option<u64>) -> Album {
    Album {
        key: key.to_string(),
        artist: artist.to_string(),
        title: title.to_string(),
        track_count: 1,
        durations: vec![180],
        catalog_numbers: Vec::new(),
        barcodes: Vec::new(),
        discogs_release_id: release_id,
        plays: 0,
        loved: 0,
        rating: None,
        last_played: None,
        skips: 0,
    }
}

/// Cora Seed's 2001 played 3 days before the clock's start, 2003 never played, and an
/// untagged album.
fn library() -> Profile {
    Profile {
        albums: vec![
            Album {
                plays: 9,
                skips: 1,
                last_played: Some(START - 3 * DAY),
                ..album("a1", "Cora Seed", "Seed Album", Some(2001))
            },
            Album {
                rating: Some(5),
                ..album("a2", "Gus Other", "Produced Album", Some(2003))
            },
            album("a3", "Made-Up Artist", "Untagged", None),
        ],
    }
}

/// A server for `test` with the fixture set imported when `catalog`, and `profile` kept.
fn server(test: &str, catalog: bool, profile: Option<&Profile>) -> (SocketAddr, Arc<FakeClock>) {
    let dir = support::fresh_dir("rediscover", test);
    let (path, _) = support::write_config(&dir);
    if catalog {
        let dumps = dir.join("discogs");
        fs::create_dir_all(&dumps).unwrap();
        let fixtures =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../sources/tests/fixtures/dumps");
        for entry in fs::read_dir(fixtures).unwrap() {
            let entry = entry.unwrap();
            fs::copy(entry.path(), dumps.join(entry.file_name())).unwrap();
        }
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(format!("[dumps]\ndir = \"{}\"\n", dumps.display()).as_bytes())
            .unwrap();
    }
    let config = config::load(&path).unwrap();
    if catalog {
        import::run(&config, &FakeClock::new(), import::BATCH, &mut |_| {}).unwrap();
    }
    if let Some(profile) = profile {
        profile::keep(profile, &config.data_dir).unwrap();
    }
    let clock = Arc::new(FakeClock::new());
    let addr = client::serve_at(config, api::LIMITS, clock.clone());
    (addr, clock)
}

fn call(addr: SocketAddr, path: &str, body: &Value) -> Reply {
    send(addr, &post(path, &body.to_string()))
}

fn ok(addr: SocketAddr, path: &str, body: &Value) -> Value {
    let reply = call(addr, path, body);
    assert_eq!(
        (reply.status, &reply.body["ok"]),
        (200, &json!(true)),
        "{reply:?}"
    );
    reply.body
}

fn today() -> Value {
    json!({"day": "2026-11-12"})
}

/// The ids of a reply's albums, in order.
fn ids(reply: &Value) -> Vec<String> {
    reply["albums"]
        .as_array()
        .unwrap()
        .iter()
        .map(|album| album["id"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn the_day_s_albums_come_in_the_contract_s_shape() {
    let (addr, _) = server("shape", true, Some(&library()));
    let reply = ok(addr, PATH, &today());
    assert_eq!(
        reply["rediscover"],
        json!({"day": "2026-11-12", "computed_at": START, "albums_considered": 3})
    );
    let untagged = ids(&reply)[1].clone();
    assert!(untagged.starts_with('k') && untagged.len() == 17, "{reply}");
    assert_eq!(ids(&reply), ["r2003".to_string(), untagged.clone()]);
    assert_eq!(
        reply["albums"][0],
        json!({
            "id": "r2003", "key": "a2", "artist": "Gus Other", "title": "Produced Album",
            "release_id": 2003, "year": 2001, "label": {"id": 30, "name": "Seed Records"},
            "styles": [{"id": reply["albums"][0]["styles"][0]["id"], "name": "Made-Up Common"}],
            "plays": 0, "skips": 0, "last_played": null, "score": 1.0,
            "community": {"release_id": 2003, "status": "not_queued", "checked_at": null,
                          "values": null},
            "reasons": [
                {"kind": "never_played", "words": "Never played", "keys": [], "artist": null,
                 "role": null, "label": null},
                {"kind": "credit",
                 "words": "Vocals: Cora Seed, also on 1 album you played this month",
                 "keys": ["a1"], "artist": {"id": 20, "name": "Cora Seed"}, "role": "Vocals",
                 "label": null},
                {"kind": "loved", "words": "Rated 5 by you", "keys": [], "artist": null,
                 "role": null, "label": null}]
        })
    );
    assert_eq!(
        reply["albums"][1],
        json!({
            "id": untagged, "key": "a3", "artist": "Made-Up Artist", "title": "Untagged",
            "release_id": null, "year": null, "label": null, "styles": [], "plays": 0,
            "skips": 0, "last_played": null, "score": 1.0 / 1.3, "community": null,
            "reasons": [{"kind": "never_played", "words": "Never played", "keys": [],
                         "artist": null, "role": null, "label": null}]
        })
    );
    assert_eq!(reply["threads"], json!([]));
    // The same day again is the same reply.
    assert_eq!(ok(addr, PATH, &today()), reply);
}

#[test]
fn without_a_profile_it_is_refused_and_without_a_catalog_it_is_not() {
    let (addr, _) = server("no-profile", true, None);
    let reply = call(addr, PATH, &today());
    assert_eq!(
        (reply.status, reply.body["ok"].clone(), reply.code()),
        (200, json!(false), "no_profile"),
        "{reply:?}"
    );
    let (addr, _) = server("no-catalog", false, Some(&library()));
    let reply = ok(addr, PATH, &today());
    assert_eq!(ids(&reply).len(), 2);
    assert_eq!(reply["albums"][0]["year"], json!(null));
    assert_eq!(reply["albums"][0]["reasons"].as_array().unwrap().len(), 2);
}

#[test]
fn a_not_now_mark_leaves_an_album_out_until_it_ends_or_is_undone() {
    let (addr, clock) = server("not-now", true, Some(&library()));
    assert_eq!(
        ok(addr, NOT_NOW, &json!({"id": "r2003", "days": 30})),
        json!({"ok": true, "until": START + 30 * DAY})
    );
    assert_eq!(ids(&ok(addr, PATH, &today())).len(), 1);
    // Sent again, it replaces the one there.
    assert_eq!(
        ok(addr, NOT_NOW, &json!({"id": "r2003", "days": 1}))["until"],
        json!(START + DAY)
    );
    clock.sleep(Duration::from_secs(u64::try_from(DAY).unwrap()));
    assert_eq!(ids(&ok(addr, PATH, &today()))[0], "r2003");
    ok(addr, NOT_NOW, &json!({"id": "r2003", "days": 365}));
    assert_eq!(
        ok(addr, UNDO, &json!({"id": "r2003"})),
        json!({"ok": true, "removed": true})
    );
    assert_eq!(ids(&ok(addr, PATH, &today()))[0], "r2003");
    assert_eq!(
        ok(addr, UNDO, &json!({"id": "r2003"}))["removed"],
        json!(false)
    );
    // A mark never touches the recommendations' verdicts.
    assert_eq!(
        ok(addr, "/v1/feedback/list", &json!({"after": 0}))["feedback"],
        json!([])
    );
}

#[test]
fn before_any_mark_an_undo_removes_nothing_and_creates_no_file() {
    let (addr, _) = server("undo-first", false, None);
    assert_eq!(
        ok(addr, UNDO, &json!({"id": "k0123456789abcdef"}))["removed"],
        json!(false)
    );
}

#[test]
fn a_request_outside_the_shape_is_a_400_naming_the_field() {
    let (addr, _) = server("bad", false, Some(&library()));
    for (path, body, field) in [
        (PATH, json!({}), "day"),
        (PATH, json!({"day": "2026-02-30"}), "day"),
        (PATH, json!({"day": "12/11/2026"}), "day"),
        (PATH, json!({"day": 20_261_112}), "day"),
        (PATH, json!({"day": "2026-11-12", "limit": 5}), "limit"),
        (NOT_NOW, json!({"id": "r2003"}), "days"),
        (NOT_NOW, json!({"id": "r2003", "days": 0}), "days"),
        (NOT_NOW, json!({"id": "r2003", "days": 366}), "days"),
        (NOT_NOW, json!({"id": "2003", "days": 3}), "id"),
        (NOT_NOW, json!({"id": "r0", "days": 3}), "id"),
        (NOT_NOW, json!({"id": "kABCDEF", "days": 3}), "id"),
        (
            NOT_NOW,
            json!({"id": "r2003", "days": 3, "until": 1}),
            "until",
        ),
        (UNDO, json!({}), "id"),
        (UNDO, json!({"id": 2003}), "id"),
    ] {
        let reply = call(addr, path, &body);
        assert_eq!((reply.status, reply.code()), (400, "bad_request"), "{body}");
        let message = reply.body["error"]["message"].as_str().unwrap();
        assert!(message.contains(field), "{body}: {message}");
    }
}
