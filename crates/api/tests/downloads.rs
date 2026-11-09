//! The `/v1/downloads/*` routes over real `TcpStream`s on `127.0.0.1`, against a store in a
//! temp folder: each route's replies and refusals as docs/api.md writes them.

#[path = "support/client.rs"]
mod client;
#[path = "../../config/tests/support/setup.rs"]
mod support;

use std::fs;
use std::io::Write as _;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use client::{Reply, post, post_with, send};
use features::download::{
    Candidate, CandidateFile, Downloads, Event, Format, NewJob, Policy, Release, Source, Track,
    Upgrade,
};
use serde_json::{Value, json};

const ROUTES: [&str; 6] = ["config", "list", "create", "retry", "cancel", "handoff"];

/// A server whose config has the downloads tables, and its data directory.
fn server(test: &str) -> (SocketAddr, PathBuf) {
    let dir = support::fresh_dir("downloads", test);
    let (path, data_dir) = support::write_config(&dir);
    let tables = format!(
        "\n[slskd]\nurl = \"http://127.0.0.1:9\"\napi_key = \"made-up-slskd-key\"\n\
         downloads_dir = \"{}\"\n[staging]\ndir = \"{}\"\n",
        dir.join("slskd").display(),
        dir.join("staging").display()
    );
    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(tables.as_bytes()).unwrap();
    (
        client::serve(config::load(&path).unwrap(), api::LIMITS),
        data_dir,
    )
}

/// A server whose config has no downloads tables.
fn off(test: &str) -> SocketAddr {
    let dir = support::fresh_dir("downloads", test);
    let (path, _) = support::write_config(&dir);
    client::serve(config::load(&path).unwrap(), api::LIMITS)
}

fn call(addr: SocketAddr, route: &str, body: &Value) -> Reply {
    send(
        addr,
        &post(&format!("/v1/downloads/{route}"), &body.to_string()),
    )
}

/// A 200 `{"ok": true}` reply's body.
fn ok(addr: SocketAddr, route: &str, body: &Value) -> Value {
    let reply = call(addr, route, body);
    assert_eq!(
        (reply.status, &reply.body["ok"]),
        (200, &json!(true)),
        "{reply:?}"
    );
    reply.body
}

/// A 200 `{"ok": false}` reply's error.
fn refused(addr: SocketAddr, route: &str, body: &Value) -> Value {
    let reply = call(addr, route, body);
    assert_eq!(reply.status, 200, "{reply:?}");
    assert_eq!(reply.body["ok"], false, "{reply:?}");
    reply.body["error"].clone()
}

fn bad_request(addr: SocketAddr, route: &str, body: &Value) {
    let reply = call(addr, route, body);
    assert_eq!(reply.status, 400, "{body}: {reply:?}");
    assert_eq!(reply.code(), "bad_request");
}

fn snapshot(release_id: i64) -> Value {
    json!({
        "release_id": release_id, "artist": "Made-Up Artist", "title": "Made-Up Album",
        "year": 1999,
        "tracklists": [[{"position": "1", "title": "First Song", "seconds": 245},
                        {"position": "2", "title": "Second Song", "seconds": null}]]
    })
}

fn create(addr: SocketAddr, release_id: i64) -> Value {
    ok(
        addr,
        "create",
        &json!({"snapshot": snapshot(release_id), "source": "manual"}),
    )["job"]
        .clone()
}

fn now() -> i64 {
    let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    i64::try_from(since.as_secs()).unwrap()
}

/// A job taken to `staged` through the store, as the runner would.
fn staged(data_dir: &Path, release_id: i64) -> i64 {
    let mut db = Downloads::open(data_dir).unwrap();
    let job = NewJob {
        release: Release {
            release_id,
            artist: "A".to_string(),
            title: "T".to_string(),
            year: None,
        },
        tracklists: vec![vec![Track {
            position: "1".to_string(),
            title: "S".to_string(),
            seconds: None,
        }]],
        format_policy: Policy::Flac,
        source: Source::Manual,
        upgrade: None,
    };
    let now = now();
    let id = db.create(&job, now).unwrap().unwrap().id;
    let candidate = Candidate {
        peer: "made-up-peer".to_string(),
        format: Format::Flac,
        bit_depth: Some(16),
        sample_rate: Some(44100),
        bitrate: None,
        variable_bitrate: None,
        tracklist: 0,
        files: vec![CandidateFile {
            remote: "@@p\\A\\01 S.flac".to_string(),
            name: "01 S.flac".to_string(),
            size: 10,
            position: Some("1".to_string()),
        }],
    };
    db.transition(id, Event::Search, now).unwrap().unwrap();
    db.found(id, &[candidate], now).unwrap().unwrap();
    db.transition(id, Event::Download, now).unwrap().unwrap();
    db.transition(id, Event::Downloaded, now).unwrap().unwrap();
    let files = [features::download::File {
        name: "01 S.flac".to_string(),
        size: 10,
        position: Some("1".to_string()),
    }];
    db.staged(id, &format!("job-{id}"), &files, now)
        .unwrap()
        .unwrap();
    id
}

fn handoff(addr: SocketAddr, id: i64, machine: &str, event: &Value) -> Reply {
    call(
        addr,
        "handoff",
        &json!({"id": id, "machine": machine, "event": event}),
    )
}

#[test]
fn every_route_needs_the_token() {
    let (addr, _) = server("token");
    for route in ROUTES {
        let reply = send(
            addr,
            &post_with(&format!("/v1/downloads/{route}"), &[], "{}"),
        );
        assert_eq!(reply.status, 401, "{route}");
    }
}

#[test]
fn every_route_but_config_is_off_without_the_tables() {
    let addr = off("off");
    for route in &ROUTES[1..] {
        assert_eq!(
            refused(addr, route, &json!({}))["code"],
            "downloads_off",
            "{route}"
        );
    }
    let config = ok(addr, "config", &json!({}));
    assert_eq!(config["enabled"], false);
    assert!(config["off_reason"].as_str().unwrap().contains("[slskd]"));
    assert_eq!(config["staging_dir_name"], Value::Null);
    assert_eq!(config["slskd_url"], Value::Null);
}

#[test]
fn config_names_the_policies_and_the_staging_folder() {
    let (addr, _) = server("config");
    let config = ok(addr, "config", &json!({}));
    assert_eq!(config["enabled"], true);
    assert_eq!(config["off_reason"], Value::Null);
    assert_eq!(
        config["format_policies"],
        json!(["flac", "lossless", "mp3_320", "any"])
    );
    assert_eq!(config["default_format_policy"], "flac");
    assert_eq!(config["staging_dir_name"], "staging");
    assert_eq!(config["slskd_url"], "http://127.0.0.1:9");
    bad_request(addr, "config", &json!({"x": 1}));
}

#[test]
fn create_queues_a_job_in_the_contract_s_shape() {
    let (addr, _) = server("create");
    let job = create(addr, 4242);
    let keys: Vec<&str> = job
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "attempt",
            "candidate",
            "cleanup_pending",
            "created_at",
            "failed_stage",
            "files",
            "format_policy",
            "id",
            "lease",
            "next_attempt_at",
            "progress",
            "reason",
            "release",
            "review",
            "revision",
            "search",
            "source",
            "staged_path",
            "state",
            "tagging",
            "updated_at",
            "upgrade"
        ]
    );
    assert_eq!(job["state"], "queued");
    assert_eq!(job["format_policy"], "flac");
    assert_eq!(job["source"], "manual");
    assert_eq!(
        job["release"],
        json!({"release_id": 4242, "artist": "Made-Up Artist", "title": "Made-Up Album",
               "year": 1999})
    );
    assert_eq!(job["tagging"], json!({"status": "none", "reasons": []}));
    assert_eq!(job["files"], json!([]));
    assert_eq!(
        (&job["attempt"], &job["cleanup_pending"]),
        (&json!(1), &json!(false))
    );

    let again = refused(
        addr,
        "create",
        &json!({"snapshot": snapshot(4242), "source": "manual"}),
    );
    assert_eq!(again["code"], "already_active");
    assert_eq!(again["job_id"], job["id"]);
}

#[test]
fn create_takes_a_joined_position_for_an_index_track() {
    let (addr, data_dir) = server("create-joined");
    let mut snapshot = snapshot(7);
    snapshot["tracklists"] = json!([[
        {"position": "1", "title": "First Song", "seconds": 245},
        {"position": "2a+2b", "title": "Second Song", "seconds": null}
    ]]);
    let job = ok(
        addr,
        "create",
        &json!({"snapshot": snapshot, "source": "manual"}),
    )["job"]
        .clone();
    let id = job["id"].as_i64().unwrap();
    let tracklists = Downloads::open(&data_dir).unwrap().tracklists(id).unwrap();
    assert_eq!(tracklists[0][1].position, "2a+2b");
}

#[test]
fn create_takes_an_upgrade_with_a_lossless_policy_only() {
    let (addr, _) = server("create-upgrade");
    let upgrade = json!({"current_quality": "MP3 320", "old_paths": ["library/A/T/01 S.mp3"]});
    let job = ok(
        addr,
        "create",
        &json!({"snapshot": snapshot(1), "source": "upgrade", "format_policy": "lossless",
                "upgrade": upgrade}),
    )["job"]
        .clone();
    assert_eq!(job["upgrade"], upgrade);
    bad_request(
        addr,
        "create",
        &json!({"snapshot": snapshot(2), "source": "upgrade", "format_policy": "mp3_320",
                "upgrade": upgrade}),
    );
    bad_request(
        addr,
        "create",
        &json!({"snapshot": snapshot(2), "source": "upgrade"}),
    );
    bad_request(
        addr,
        "create",
        &json!({"snapshot": snapshot(2), "source": "manual", "upgrade": upgrade}),
    );
}

#[test]
fn create_refuses_a_snapshot_outside_the_contract() {
    let (addr, _) = server("create-bad");
    let mut cases = Vec::new();
    let mut with = |change: &dyn Fn(&mut Value)| {
        let mut body = json!({"snapshot": snapshot(7), "source": "manual"});
        change(&mut body);
        cases.push(body);
    };
    with(&|b| b["extra"] = json!(1));
    with(&|b| b["snapshot"]["extra"] = json!(1));
    with(&|b| b["snapshot"]["release_id"] = json!(0));
    with(&|b| b["snapshot"]["artist"] = json!(""));
    with(&|b| b["snapshot"]["title"] = json!("x".repeat(1001)));
    with(&|b| b["snapshot"]["tracklists"] = json!([]));
    with(&|b| b["snapshot"]["tracklists"][0][1]["position"] = json!("1"));
    with(&|b| b["snapshot"]["tracklists"][0][0]["position"] = json!(""));
    with(&|b| b["snapshot"]["tracklists"][0][0]["seconds"] = json!(-1));
    with(&|b| {
        b["snapshot"].as_object_mut().unwrap().remove("year");
    });
    with(&|b| b["source"] = json!("bot"));
    with(&|b| b["format_policy"] = json!("wav"));
    for body in cases {
        bad_request(addr, "create", &body);
    }
}

#[test]
fn list_pages_by_revision_and_resets_on_another_epoch() {
    let (addr, _) = server("list");
    let first = ok(addr, "list", &json!({"epoch": null, "after": 0}));
    assert_eq!(
        (&first["reset"], &first["more"], &first["jobs"]),
        (&json!(false), &json!(false), &json!([]))
    );
    let epoch = first["epoch"].as_str().unwrap().to_string();
    let one = create(addr, 1);
    let two = create(addr, 2);
    let listed = ok(addr, "list", &json!({"epoch": epoch, "after": 0}));
    let ids: Vec<&Value> = listed["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| &j["id"])
        .collect();
    assert_eq!(ids, [&one["id"], &two["id"]]);
    assert_eq!(listed["revision"], two["revision"]);
    let after = ok(
        addr,
        "list",
        &json!({"epoch": epoch, "after": one["revision"]}),
    );
    assert_eq!(after["jobs"].as_array().unwrap().len(), 1);
    let caught_up = ok(
        addr,
        "list",
        &json!({"epoch": epoch, "after": two["revision"]}),
    );
    assert_eq!(caught_up["jobs"], json!([]));
    let other = ok(
        addr,
        "list",
        &json!({"epoch": "0000000000000000", "after": 2}),
    );
    assert_eq!(other["reset"], true);
    assert_eq!(other["jobs"].as_array().unwrap().len(), 2);
    bad_request(addr, "list", &json!({"after": 0}));
    bad_request(addr, "list", &json!({"epoch": null, "after": -1}));
}

#[test]
fn list_stops_a_page_at_200_jobs() {
    let (addr, data_dir) = server("list-200");
    let mut db = Downloads::open(&data_dir).unwrap();
    for release_id in 1..=201 {
        let job = NewJob {
            release: Release {
                release_id,
                artist: "A".to_string(),
                title: "T".to_string(),
                year: None,
            },
            tracklists: vec![vec![Track {
                position: "1".to_string(),
                title: "S".to_string(),
                seconds: None,
            }]],
            format_policy: Policy::Flac,
            source: Source::Manual,
            upgrade: None,
        };
        db.create(&job, now()).unwrap().unwrap();
    }
    let page = ok(addr, "list", &json!({"epoch": null, "after": 0}));
    let jobs = page["jobs"].as_array().unwrap();
    assert_eq!((jobs.len(), &page["more"]), (200, &json!(true)));
    let last = &jobs[199]["revision"];
    let rest = ok(
        addr,
        "list",
        &json!({"epoch": page["epoch"], "after": last}),
    );
    assert_eq!(rest["jobs"].as_array().unwrap().len(), 1);
    assert_eq!(rest["more"], false);
}

#[test]
fn list_stops_a_page_before_768_kb_with_one_job_at_least() {
    let (addr, data_dir) = server("list-size");
    let mut db = Downloads::open(&data_dir).unwrap();
    for release_id in 1..=3 {
        let job = NewJob {
            release: Release {
                release_id,
                artist: "A".to_string(),
                title: "T".to_string(),
                year: None,
            },
            tracklists: vec![vec![Track {
                position: "1".to_string(),
                title: "S".to_string(),
                seconds: None,
            }]],
            format_policy: Policy::Flac,
            source: Source::Upgrade,
            upgrade: Some(Upgrade {
                current_quality: "MP3".to_string(),
                old_paths: vec!["p".repeat(1000); 500],
            }),
        };
        db.create(&job, now()).unwrap().unwrap();
    }
    let page = ok(addr, "list", &json!({"epoch": null, "after": 0}));
    assert_eq!(page["jobs"].as_array().unwrap().len(), 1);
    assert_eq!(page["more"], true);
}

#[test]
fn retry_and_cancel_answer_the_job_or_the_refusal() {
    let (addr, data_dir) = server("retry-cancel");
    assert_eq!(
        refused(addr, "retry", &json!({"id": 99}))["code"],
        "no_such_job"
    );
    assert_eq!(
        refused(addr, "cancel", &json!({"id": 99}))["code"],
        "no_such_job"
    );
    let queued = create(addr, 1);
    let error = refused(addr, "retry", &json!({"id": queued["id"]}));
    assert_eq!(
        (&error["code"], &error["state"]),
        (&json!("illegal_transition"), &json!("queued"))
    );
    let cancelled = ok(addr, "cancel", &json!({"id": queued["id"]}))["job"].clone();
    assert_eq!(cancelled["state"], "cancelled");
    let again = ok(addr, "cancel", &json!({"id": queued["id"]}))["job"].clone();
    assert_eq!(again, cancelled);

    let id = staged(&data_dir, 2);
    let mut db = Downloads::open(&data_dir).unwrap();
    db.fail(id, features::download::Stage::Tag, "x", now())
        .unwrap()
        .unwrap();
    let error = refused(addr, "retry", &json!({"id": id, "format_policy": "any"}));
    assert_eq!(error["code"], "illegal_transition");
    let retried = ok(addr, "retry", &json!({"id": id}))["job"].clone();
    assert_eq!(retried["state"], "staged");
    bad_request(addr, "retry", &json!({"id": id, "format_policy": "wav"}));
    bad_request(addr, "cancel", &json!({"id": "1"}));
}

#[test]
fn handoff_checks_the_lease_and_the_claim() {
    let (addr, data_dir) = server("handoff");
    let id = staged(&data_dir, 1);
    let lease = handoff(addr, id, "Living Room", &json!({"type": "lease"}));
    assert_eq!(lease.body["job"]["lease"]["machine"], "Living Room");
    let other = handoff(addr, id, "Desk", &json!({"type": "lease"}));
    let error = &other.body["error"];
    assert_eq!(
        (&error["code"], &error["machine"]),
        (&json!("claimed"), &json!("Living Room"))
    );
    assert!(error["expires_at"].as_i64().unwrap() > now());

    let clean = json!({"type": "tagging", "status": "clean", "reasons": []});
    let tagged = handoff(addr, id, "Living Room", &clean);
    assert_eq!(tagged.body["job"]["tagging"]["status"], "clean");
    let approve = json!({"type": "approved"});
    assert_eq!(
        handoff(addr, id, "Living Room", &approve).body["error"]["code"],
        "not_claimed"
    );
    handoff(addr, id, "Living Room", &json!({"type": "claim"}));
    assert_eq!(
        handoff(addr, id, "Desk", &approve).body["error"]["code"],
        "claimed"
    );
    let approved = handoff(addr, id, "Living Room", &approve);
    assert_eq!(approved.body["job"]["state"], "approved");
    // A repeat is answered with the job unchanged.
    let repeat = handoff(addr, id, "Living Room", &approve);
    assert_eq!(repeat.body["job"], approved.body["job"]);
    let take_over = json!({"type": "claim", "take_over": true});
    assert_eq!(
        handoff(addr, id, "Desk", &take_over).body["job"]["review"]["machine"],
        "Desk"
    );
    let imported = handoff(addr, id, "Desk", &json!({"type": "imported"}));
    assert_eq!(imported.body["job"]["state"], "imported");
    assert_eq!(
        handoff(addr, id, "Desk", &json!({"type": "rejected"})).body["error"]["state"],
        "imported"
    );
}

#[test]
fn handoff_refuses_an_event_outside_the_contract() {
    let (addr, data_dir) = server("handoff-bad");
    let id = staged(&data_dir, 1);
    let no_job = handoff(addr, 99, "Desk", &json!({"type": "lease"}));
    assert_eq!(no_job.body["error"]["code"], "no_such_job");
    for (machine, event) in [
        ("Desk", json!({"type": "dance"})),
        ("Desk", json!({"type": "lease", "x": 1})),
        ("Desk", json!({"type": "tagging", "status": "clean"})),
        (
            "Desk",
            json!({"type": "tagging", "status": "fine", "reasons": []}),
        ),
        (
            "Desk",
            json!({"type": "failed", "stage": "search", "reason": "x"}),
        ),
        ("Desk", json!({"type": "claim", "take_over": "yes"})),
        ("", json!({"type": "lease"})),
        (&*"m".repeat(65), json!({"type": "lease"})),
    ] {
        bad_request(
            addr,
            "handoff",
            &json!({"id": id, "machine": machine, "event": event}),
        );
    }
    let failed = json!({"type": "failed", "stage": "tag", "reason": "unreadable file"});
    handoff(addr, id, "Desk", &json!({"type": "lease"}));
    let job = &handoff(addr, id, "Desk", &failed).body["job"];
    assert_eq!(
        (&job["state"], &job["failed_stage"]),
        (&json!("failed"), &json!("tag"))
    );
    assert_eq!(job["reason"], "unreadable file");
}
