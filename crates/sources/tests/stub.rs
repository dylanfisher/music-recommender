//! The stub server itself: it records what the slskd client will send, a method and a body,
//! and not only a path and headers.

#[path = "support/stub.rs"]
mod stub;

use stub::{Stub, reply};

#[test]
fn records_the_method_and_the_body() {
    let stub = Stub::start(|_| reply(201, &[], "{}"));
    let body = r#"{"username":"made-up-peer","files":[]}"#;
    ureq::post(&format!("{}/api/v0/transfers/downloads/batches", stub.url))
        .header("Content-Type", "application/json")
        .send(body)
        .unwrap();
    let request = stub.next_request();
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/api/v0/transfers/downloads/batches");
    assert_eq!(request.body, body.as_bytes());
}
