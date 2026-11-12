//! `POST /v1/sync/status` over real `TcpStream`s on `127.0.0.1`, against fixture databases
//! at a made-up time: the same values `sync status` prints, in the shape docs/api.md writes.

#[path = "support/client.rs"]
mod client;
#[path = "../../features/tests/support/clock.rs"]
mod clock;
#[path = "../../config/tests/support/setup.rs"]
mod support;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use client::{Reply, post, send};
use clock::{FakeClock, START};
use config::Config;
use features::status::{Community, Request, Tier, Values};
use serde_json::{Value, json};

/// A last error holding characters JSON has to escape.
const HOSTILE: &str = r#"<b> "double" 'single' & \ more"#;

const VALUES: Values = Values {
    rating_average: 3.5,
    rating_count: 4,
    have: 10,
    want: 2,
    num_for_sale: None,
    lowest_price: None,
};

/// A server on a config written for `test`, its clock at `START`, and the config. With
/// `dumps`, the config has a `[dumps]` table over a copy of the sources crate's fixture set;
/// the folder is returned.
fn start(test: &str, with_dumps: bool) -> (SocketAddr, Config, PathBuf) {
    let dir = support::fresh_dir("sync-status", test);
    let (path, _) = support::write_config(&dir);
    let folder = dir.join("discogs");
    if with_dumps {
        std::fs::create_dir_all(&folder).unwrap();
        let set = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../sources/tests/fixtures/dumps");
        for entry in std::fs::read_dir(set).unwrap() {
            let entry = entry.unwrap();
            std::fs::copy(entry.path(), folder.join(entry.file_name())).unwrap();
        }
        let toml = std::fs::read_to_string(&path).unwrap()
            + &format!("[dumps]\ndir = \"{}\"\n", folder.display());
        std::fs::write(&path, toml).unwrap();
    }
    let addr = client::serve_at(
        config::load(&path).unwrap(),
        api::LIMITS,
        Arc::new(FakeClock::new()),
    );
    (addr, config::load(&path).unwrap(), folder)
}

fn status(addr: SocketAddr, body: &Value) -> Reply {
    send(addr, &post("/v1/sync/status", &body.to_string()))
}

fn ok(addr: SocketAddr) -> Value {
    let reply = status(addr, &json!({}));
    assert_eq!(
        (reply.status, &reply.body["ok"]),
        (200, &json!(true)),
        "{reply:?}"
    );
    reply.body
}

/// The library's releases 1 to 4 (fetched, gone, failed, never fetched) and the candidates'
/// 10 and 11 (fetched 91 days ago and so due, never fetched), and a last error.
fn fixture(config: &Config) {
    let mut db = Community::open(&config.data_dir).unwrap();
    db.queue_releases(&[1, 2, 3, 4], Tier::Library).unwrap();
    db.queue_releases(&[10, 11], Tier::Ratings).unwrap();
    db.record(1, VALUES, "{}", START - 600).unwrap();
    db.record_gone(Request::release(2), START - 600).unwrap();
    for attempt in 0..10 {
        db.record_failure(Request::release(3), HOSTILE, START - 300 + attempt)
            .unwrap();
    }
    db.record(10, VALUES, "{}", START - 91 * 24 * 3600).unwrap();
    db.record_error(HOSTILE, START - 120).unwrap();
}

/// The dumps part of a status with the dumps off and nothing imported.
fn no_dumps() -> Value {
    let absent = json!({"state": "none", "date": null, "swapped_at": null, "error": null});
    json!({"dir": null, "newest_set": null, "newest_set_error": null,
           "running": false, "running_error": null,
           "catalog": absent, "build": absent, "records": []})
}

#[test]
fn answers_the_counts_the_sync_status_reads() {
    let (addr, config, _) = start("counts", false);
    fixture(&config);
    assert_eq!(
        ok(addr),
        json!({"ok": true, "at": START,
               "sync": {
                   "paused": false, "running": false, "other_shape": false,
                   "yielding_since": null,
                   "library": {"queued": 1, "with_values": 1, "failed": 1, "gone": 1, "due": 0},
                   "candidates": {"queued": 1, "with_values": 1, "failed": 0, "gone": 0,
                                  "due": 1},
                   "failures": [{"release_id": 3, "error": HOSTILE}],
                   "requests_last_hour": 0,
                   "last_error": {"at": START - 120, "message": HOSTILE},
                   "seconds_left": 3,
                   "requests_per_minute": 60},
               "dumps": no_dumps()})
    );
}

#[test]
fn answers_zero_counts_and_no_error_before_a_sync_has_run() {
    let (addr, _, _) = start("empty", false);
    let body = ok(addr);
    assert_eq!(body["sync"]["library"]["queued"], 0, "{body}");
    assert_eq!(body["sync"]["last_error"], Value::Null, "{body}");
    assert_eq!(body["sync"]["failures"], json!([]), "{body}");
    assert_eq!(body["sync"]["seconds_left"], 0, "{body}");
}

#[test]
fn answers_the_yield_while_a_sync_holds_the_lock() {
    let (addr, config, _) = start("yield", false);
    let db = Community::open(&config.data_dir).unwrap();
    db.set_yielding(Some(START - 300)).unwrap();
    let body = ok(addr);
    assert_eq!(body["sync"]["yielding_since"], Value::Null, "{body}");
    let _lock = Community::lock(&config.data_dir).unwrap();
    let body = ok(addr);
    assert_eq!(body["sync"]["running"], true, "{body}");
    assert_eq!(body["sync"]["yielding_since"], START - 300, "{body}");
}

#[test]
fn answers_the_dumps_and_the_catalog_in_use() {
    let (addr, config, folder) = start("catalog", true);
    features::import::run(&config, &FakeClock::new(), 100, &mut |_| {}).unwrap();
    let body = ok(addr);
    let present = json!({"state": "present", "date": "20000101", "swapped_at": START,
                         "error": null});
    let row = |dump: &str, records: u64| {
        json!({"dump": dump, "building_records": null, "building_loaded": null,
               "catalog_records": records})
    };
    assert_eq!(
        body["dumps"],
        json!({"dir": folder.display().to_string(),
               "newest_set": "20000101", "newest_set_error": null,
               "running": false, "running_error": null,
               "catalog": present,
               "build": {"state": "none", "date": null, "swapped_at": null, "error": null},
               "records": [row("artists", 10), row("labels", 5), row("masters", 2),
                           row("releases", 12)]}),
        "{body}"
    );
}

#[test]
fn answers_what_cant_be_read_as_errors_not_a_500() {
    let (addr, config, folder) = start("unreadable", true);
    std::fs::remove_dir_all(&folder).unwrap();
    std::fs::write(config.data_dir.join("catalog.sqlite"), vec![0xA5; 8192]).unwrap();
    let body = ok(addr);
    let dumps = &body["dumps"];
    let error = dumps["newest_set_error"].as_str().unwrap();
    assert!(error.contains(&folder.display().to_string()), "{body}");
    assert_eq!(dumps["newest_set"], Value::Null, "{body}");
    assert_eq!(dumps["catalog"]["state"], "error", "{body}");
    assert!(
        dumps["catalog"]["error"]
            .as_str()
            .unwrap()
            .contains("not a database"),
        "{body}"
    );
    assert_eq!(dumps["catalog"]["date"], Value::Null, "{body}");
}

#[test]
fn refuses_any_field_and_a_body_that_isnt_an_object() {
    let (addr, _, _) = start("bad", false);
    for body in [json!({"x": 1}), json!({"release_ids": []}), json!([])] {
        let reply = status(addr, &body);
        assert_eq!((reply.status, reply.code()), (400, "bad_request"), "{body}");
    }
}
