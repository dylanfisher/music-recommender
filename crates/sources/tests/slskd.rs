//! The slskd client against a stub server with the hand-written replies in
//! `fixtures/slskd/`, never a real slskd.

#[path = "support/stub.rs"]
mod stub;

use std::time::Duration;

use serde_json::{Value, json};
use sources::slskd::client::{Client, Error, Queued, TransferState};
use stub::{Stub, reply};

const KEY: &str = "made-up-key";
const SEARCH_ID: &str = "5b3e1f0a-8c2d-4e6f-9a1b-2c3d4e5f6a7b";

fn fixture(name: &str) -> String {
    std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/slskd")
            .join(name),
    )
    .unwrap()
}

/// A stub answering every request with `status` and the fixture `name`.
fn serving(status: u16, name: &str) -> Stub {
    let body = fixture(name);
    Stub::start(move |_| reply(status, &[], &body))
}

fn body(request: &stub::Request) -> Value {
    serde_json::from_slice(&request.body).unwrap()
}

#[test]
fn starting_a_search_sends_the_text_and_the_timeout_in_milliseconds() {
    let stub = serving(200, "search-started.json");
    let search = Client::new(&stub.url, KEY)
        .start_search("made up artist made up album", Duration::from_secs(15))
        .unwrap();
    assert_eq!(search.id, SEARCH_ID);
    assert!(!search.complete);
    assert_eq!(search.responses, Vec::new());
    let request = stub.next_request();
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/api/v0/searches");
    assert_eq!(request.header("x-api-key"), Some(KEY));
    assert_eq!(request.header("content-type"), Some("application/json"));
    assert_eq!(
        body(&request),
        json!({"searchText": "made up artist made up album", "searchTimeout": 15000})
    );
}

#[test]
fn a_completed_search_has_each_peer_and_its_files() {
    let stub = serving(200, "search-completed.json");
    let search = Client::new(&stub.url, KEY).search(SEARCH_ID).unwrap();
    let request = stub.next_request();
    assert_eq!(request.method, "GET");
    assert_eq!(
        request.path,
        format!("/api/v0/searches/{SEARCH_ID}?includeResponses=true")
    );
    assert!(search.complete);
    let peers: Vec<_> = search
        .responses
        .iter()
        .map(|r| r.username.as_str())
        .collect();
    assert_eq!(
        peers,
        [
            "made-up-peer-flac",
            "made-up-peer-mp3",
            "made-up-peer-locked"
        ]
    );
    let flac = &search.responses[0];
    assert!(flac.has_free_upload_slot);
    assert_eq!((flac.queue_length, flac.upload_speed), (0, 2_457_600));
    assert_eq!(flac.files.len(), 5);
    let first = &flac.files[0];
    assert_eq!(
        first.filename,
        "@@kq3zv\\Music\\Made-Up Artist\\Made-Up Album (1999) [FLAC]\\01 - First Song.flac"
    );
    assert_eq!(
        (first.size, first.length, first.bit_depth, first.sample_rate),
        (24_117_248, Some(245), Some(16), Some(44100))
    );
    assert_eq!(first.extension, "flac");
    assert!(!first.locked);
    // A file the peer reported no extension or length for.
    assert_eq!(flac.files[2].extension, "");
    assert_eq!(flac.files[4].length, None);

    let mp3 = &search.responses[1];
    assert!(!mp3.has_free_upload_slot);
    assert_eq!(
        (mp3.files[0].bit_rate, mp3.files[0].variable_bit_rate),
        (Some(258), Some(true))
    );

    let locked = &search.responses[2];
    assert_eq!(locked.queue_length, 740);
    assert_eq!(locked.files.len(), 2);
    assert_eq!(locked.locked_files.len(), 2);
    assert!(locked.locked_files.iter().all(|f| f.locked));
    assert_eq!(locked.files[1].length, None);
}

#[test]
fn the_responses_alone_read_the_same() {
    let stub = serving(200, "search-responses.json");
    let responses = Client::new(&stub.url, KEY).responses(SEARCH_ID).unwrap();
    assert_eq!(
        stub.next_request().path,
        format!("/api/v0/searches/{SEARCH_ID}/responses")
    );
    let completed = serving(200, "search-completed.json");
    let search = Client::new(&completed.url, KEY).search(SEARCH_ID).unwrap();
    assert_eq!(responses, search.responses);
}

#[test]
fn a_search_with_no_responses() {
    let stub = serving(200, "search-completed-empty.json");
    let search = Client::new(&stub.url, KEY).search(SEARCH_ID).unwrap();
    assert!(search.complete);
    assert_eq!(search.responses, Vec::new());
}

#[test]
fn cancelling_and_deleting_a_search() {
    let stub = Stub::start(|_| reply(204, &[], ""));
    let client = Client::new(&stub.url, KEY);
    client.cancel_search(SEARCH_ID).unwrap();
    client.delete_search(SEARCH_ID).unwrap();
    let cancel = stub.next_request();
    assert_eq!(
        (cancel.method.as_str(), cancel.path.as_str()),
        ("PUT", format!("/api/v0/searches/{SEARCH_ID}").as_str())
    );
    let delete = stub.next_request();
    assert_eq!(
        (delete.method.as_str(), delete.path.as_str()),
        ("DELETE", format!("/api/v0/searches/{SEARCH_ID}").as_str())
    );
}

#[test]
fn a_429_means_another_search_is_starting() {
    let stub = serving(429, "too-many-requests.json");
    let error = Client::new(&stub.url, KEY)
        .start_search("x", Duration::from_secs(15))
        .unwrap_err();
    assert_eq!(error, Error::Busy);
}

#[test]
fn a_401_is_a_refused_key() {
    let stub = Stub::start(|_| reply(401, &[], ""));
    assert_eq!(
        Client::new(&stub.url, KEY).search(SEARCH_ID).unwrap_err(),
        Error::Refused(401)
    );
}

#[test]
fn enqueueing_sends_the_files_and_the_destination() {
    let stub = serving(201, "batch-created.json");
    let files = [
        (
            "@@kq3zv\\Music\\Made-Up Artist\\Made-Up Album (1999) [FLAC]\\01 - First Song.flac"
                .to_string(),
            24_117_248,
        ),
        ("@@kq3zv\\Music\\x\\02.flac".to_string(), 1),
    ];
    let queued = Client::new(&stub.url, KEY)
        .enqueue("made-up-peer-flac", &files, "job-42")
        .unwrap();
    assert_eq!(
        queued,
        Queued {
            batch_id: "9d8c7b6a-5f4e-4d3c-8b2a-1f0e9d8c7b6a".to_string(),
            failures: Vec::new(),
        }
    );
    let request = stub.next_request();
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/api/v0/transfers/downloads/batches");
    assert_eq!(
        body(&request),
        json!({
            "username": "made-up-peer-flac",
            "files": [
                {"filename": files[0].0, "size": 24_117_248},
                {"filename": files[1].0, "size": 1}
            ],
            "options": {"destination": "job-42"}
        })
    );
}

#[test]
fn a_partial_or_failed_batch_lists_its_failures() {
    let partial = serving(207, "batch-partial.json");
    let queued = Client::new(&partial.url, KEY)
        .enqueue("made-up-peer-flac", &[], "job-42")
        .unwrap();
    assert_eq!(queued.failures.len(), 1);
    assert_eq!(queued.failures[0].1, "Skipped: Already in progress");
    let failed = serving(200, "batch-failed.json");
    let queued = Client::new(&failed.url, KEY)
        .enqueue("made-up-peer-flac", &[], "job-42")
        .unwrap();
    assert_eq!(queued.failures.len(), 4);
    assert!(queued.failures[3].0.ends_with("04 - The Long One.flac"));
}

#[test]
fn an_offline_user_is_not_found_with_slskd_s_words() {
    let stub = serving(404, "user-offline.json");
    assert_eq!(
        Client::new(&stub.url, KEY)
            .enqueue("made-up-peer-flac", &[], "job-42")
            .unwrap_err(),
        Error::NotFound("User made-up-peer-flac appears to be offline".to_string())
    );
}

#[test]
fn a_plain_text_404_is_read_as_it_is() {
    let stub = Stub::start(|_| reply(404, &[], "User made-up-peer-flac appears to be offline"));
    assert_eq!(
        Client::new(&stub.url, KEY)
            .enqueue("made-up-peer-flac", &[], "job-42")
            .unwrap_err(),
        Error::NotFound("User made-up-peer-flac appears to be offline".to_string())
    );
}

#[test]
fn a_user_s_transfers_in_every_state() {
    let stub = serving(200, "downloads-user.json");
    let transfers = Client::new(&stub.url, KEY)
        .downloads("made-up-peer-flac")
        .unwrap();
    assert_eq!(
        stub.next_request().path,
        "/api/v0/transfers/downloads/made-up-peer-flac"
    );
    let states: Vec<_> = transfers.iter().map(|t| t.state).collect();
    assert_eq!(
        states,
        [
            TransferState::Succeeded,
            TransferState::InProgress,
            TransferState::QueuedRemotely,
            TransferState::QueuedLocally,
            TransferState::Requested,
            TransferState::Initializing,
            TransferState::Errored,
            TransferState::Rejected,
            TransferState::Cancelled,
            TransferState::TimedOut,
            TransferState::Aborted,
        ]
    );
    let second = &transfers[1];
    assert_eq!(second.id, "00000002-aaaa-4bbb-8ccc-dddddddddddd");
    assert_eq!(
        (second.size, second.bytes_transferred),
        (31_457_280, 10_485_760)
    );
    assert_eq!(
        transfers[6].exception.as_deref(),
        Some("Transfer failed: Remote connection closed")
    );
}

#[test]
fn cancelling_a_download_removes_it() {
    let stub = Stub::start(|_| reply(204, &[], ""));
    Client::new(&stub.url, KEY)
        .cancel_download("made-up-peer-flac", "00000002-aaaa-4bbb-8ccc-dddddddddddd")
        .unwrap();
    let request = stub.next_request();
    assert_eq!(request.method, "DELETE");
    assert_eq!(
        request.path,
        "/api/v0/transfers/downloads/made-up-peer-flac/00000002-aaaa-4bbb-8ccc-dddddddddddd?remove=true"
    );
}

#[test]
fn a_user_name_is_escaped_in_the_path() {
    let stub = Stub::start(|_| reply(200, &[], r#"{"username": "a b/c", "directories": []}"#));
    Client::new(&stub.url, KEY).downloads("a b/c").unwrap();
    assert_eq!(
        stub.next_request().path,
        "/api/v0/transfers/downloads/a%20b%2Fc"
    );
}

#[test]
fn a_malformed_or_oversize_reply_is_a_bad_reply() {
    let stub = serving(200, "malformed.json");
    assert!(matches!(
        Client::new(&stub.url, KEY).search(SEARCH_ID),
        Err(Error::BadReply(_))
    ));
    let huge = format!("[{}]", " ".repeat(9 * 1024 * 1024));
    let stub = Stub::start(move |_| reply(200, &[], &huge));
    let error = Client::new(&stub.url, KEY)
        .responses(SEARCH_ID)
        .unwrap_err();
    assert!(
        matches!(&error, Error::BadReply(text) if text.contains("8388608")),
        "{error:?}"
    );
}

#[test]
fn an_unknown_transfer_state_is_refused_naming_it() {
    let stub = Stub::start(|_| {
        reply(
            200,
            &[],
            r#"{"username": "u", "directories": [{"directory": "d", "files": [
                {"id": "1", "username": "u", "filename": "d\\f.flac", "size": 1,
                 "state": "Completed, Teleported", "bytesTransferred": 0}]}]}"#,
        )
    });
    let error = Client::new(&stub.url, KEY).downloads("u").unwrap_err();
    assert!(
        matches!(&error, Error::BadReply(text) if text.contains("Teleported")),
        "{error:?}"
    );
}

#[test]
fn a_server_error_is_trouble() {
    let stub = Stub::start(|_| reply(500, &[], "boom"));
    assert!(matches!(
        Client::new(&stub.url, KEY).search(SEARCH_ID),
        Err(Error::Trouble(_))
    ));
}
