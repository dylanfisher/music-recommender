//! `POST /v1/feedback`, `/v1/feedback/undo` and `/v1/feedback/list` over real `TcpStream`s on
//! `127.0.0.1`, with a made-up clock: each reply and refusal as docs/api.md writes them.

#[path = "support/client.rs"]
mod client;
#[path = "../../features/tests/support/clock.rs"]
mod clock;
#[path = "../../config/tests/support/setup.rs"]
mod support;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use client::{Reply, post, send};
use clock::{FakeClock, START};
use features::clock::Clock;
use serde_json::{Value, json};

/// A server on a config written for `test`, with its clock.
fn server(test: &str) -> (SocketAddr, Arc<FakeClock>) {
    let dir = support::fresh_dir("feedback", test);
    let (path, _) = support::write_config(&dir);
    let clock = Arc::new(FakeClock::new());
    let addr = client::serve_at(config::load(&path).unwrap(), api::LIMITS, clock.clone());
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

#[test]
fn sets_undoes_and_lists_verdicts_on_releases_and_masters() {
    let (addr, clock) = server("set");
    let set = json!({"kind": "release", "id": 4242, "verdict": "not_interested"});
    let mark = json!({"seq": 1, "kind": "release", "id": 4242, "verdict": "not_interested",
                      "at": START});
    assert_eq!(
        ok(addr, "/v1/feedback", &set),
        json!({"ok": true, "feedback": mark})
    );
    clock.sleep(Duration::from_secs(60));
    // The same again is answered unchanged, so a replay is harmless.
    assert_eq!(ok(addr, "/v1/feedback", &set)["feedback"], mark);
    let master = ok(
        addr,
        "/v1/feedback",
        &json!({"kind": "master", "id": 4242, "verdict": "already_have"}),
    )["feedback"]
        .clone();
    assert_eq!(
        master,
        json!({"seq": 2, "kind": "master", "id": 4242, "verdict": "already_have",
               "at": START + 60})
    );
    assert_eq!(
        ok(addr, "/v1/feedback/list", &json!({"after": 0})),
        json!({"ok": true, "feedback": [mark, master], "more": false})
    );
    assert_eq!(
        ok(addr, "/v1/feedback/list", &json!({"after": 1}))["feedback"],
        json!([master])
    );
    let undo = json!({"kind": "release", "id": 4242});
    assert_eq!(
        ok(addr, "/v1/feedback/undo", &undo),
        json!({"ok": true, "removed": true})
    );
    assert_eq!(
        ok(addr, "/v1/feedback/undo", &undo),
        json!({"ok": true, "removed": false})
    );
    assert_eq!(
        ok(addr, "/v1/feedback/list", &json!({"after": 0}))["feedback"],
        json!([master])
    );
}

#[test]
fn refuses_a_body_outside_the_contract() {
    let (addr, _) = server("refuses");
    let cases = [
        (
            "/v1/feedback",
            json!({"kind": "artist", "id": 1, "verdict": "not_interested"}),
        ),
        (
            "/v1/feedback",
            json!({"kind": "release", "id": 1, "verdict": "maybe"}),
        ),
        (
            "/v1/feedback",
            json!({"kind": "release", "id": 0, "verdict": "not_interested"}),
        ),
        (
            "/v1/feedback",
            json!({"kind": "release", "verdict": "not_interested"}),
        ),
        (
            "/v1/feedback",
            json!({"kind": "release", "id": 1, "verdict": "already_have", "x": 1}),
        ),
        (
            "/v1/feedback/undo",
            json!({"kind": "release", "id": 1, "verdict": "already_have"}),
        ),
        ("/v1/feedback/undo", json!({"kind": "release", "id": "1"})),
        ("/v1/feedback/list", json!({"after": -1})),
        ("/v1/feedback/list", json!({})),
        ("/v1/feedback/list", json!({"after": 0, "limit": 10})),
    ];
    for (path, body) in cases {
        let reply = call(addr, path, &body);
        assert_eq!(
            (reply.status, reply.code()),
            (400, "bad_request"),
            "{path} {body}"
        );
    }
    assert_eq!(call(addr, "/v1/feedback/other", &json!({})).status, 404);
}
