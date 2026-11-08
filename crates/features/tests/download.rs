//! The download runner against a made-up slskd on the stub server, with a made-up clock and
//! temp folders: nothing waits on real time, and nothing reaches a real slskd or the NAS.

#[path = "support/clock.rs"]
mod clock;
#[path = "../../sources/tests/support/stub.rs"]
mod stub;
#[path = "../../config/tests/support/setup.rs"]
mod support;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use catalog::download::{Downloads, Event, NewJob, Policy, Release, Source, Stage, State, Track};
use clock::FakeClock;
use features::clock::Clock;
use features::download::{RunError, Runner};
use serde_json::{Value, json};
use sources::slskd::client::Client;
use stub::{Stub, reply};

const HOUR: i64 = 60 * 60;
const PEER: &str = "made-up-peer";
const FOLDER: &str = "@@abc\\Music\\Made-Up Artist\\Made-Up Album (1999) [FLAC]";
const FIRST_QUERY: &str = "made up artist made up album";

/// A transfer the made-up slskd holds.
#[derive(Clone, Debug)]
struct Transfer {
    id: String,
    filename: String,
    size: u64,
    state: String,
    bytes: u64,
}

/// What the made-up slskd knows and what it was asked.
#[derive(Default)]
struct Slskd {
    /// Each search text's peer responses; a text not here gets none.
    responses: HashMap<String, Vec<Value>>,
    /// Answers 429 to this many new searches first.
    busy: u32,
    /// The texts searched for, in order.
    searched: Vec<String>,
    /// Each user's transfers.
    transfers: HashMap<String, Vec<Transfer>>,
    /// Each batch enqueued: the user, its files and the destination.
    batches: Vec<(String, Vec<String>, String)>,
    /// Each transfer cancelled, by id.
    cancelled: Vec<String>,
    next_id: u32,
}

impl Slskd {
    fn answer(&mut self, request: &stub::Request) -> String {
        let path = request.path.strip_prefix("/api/v0").unwrap();
        let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
        match (request.method.as_str(), path) {
            ("POST", "/searches") => {
                if self.busy > 0 {
                    self.busy -= 1;
                    return reply(429, &[], "");
                }
                let text = body["searchText"].as_str().unwrap().to_string();
                self.searched.push(text.clone());
                let started = json!({"id": text, "isComplete": false, "responses": []});
                reply(200, &[], &started.to_string())
            }
            ("GET", _) if path.starts_with("/searches/") => {
                let text = path
                    .strip_prefix("/searches/")
                    .unwrap()
                    .split('?')
                    .next()
                    .unwrap()
                    .replace("%20", " ");
                let responses = self.responses.get(&text).cloned().unwrap_or_default();
                let search = json!({"id": text, "isComplete": true, "responses": responses});
                reply(200, &[], &search.to_string())
            }
            ("DELETE" | "PUT", _) if path.starts_with("/searches/") => reply(204, &[], ""),
            ("POST", "/transfers/downloads/batches") => {
                let username = body["username"].as_str().unwrap().to_string();
                let mut names = Vec::new();
                for file in body["files"].as_array().unwrap() {
                    self.next_id += 1;
                    let filename = file["filename"].as_str().unwrap().to_string();
                    names.push(filename.clone());
                    self.transfers
                        .entry(username.clone())
                        .or_default()
                        .push(Transfer {
                            id: format!("t{}", self.next_id),
                            filename,
                            size: file["size"].as_u64().unwrap(),
                            state: "Queued, Remotely".to_string(),
                            bytes: 0,
                        });
                }
                let destination = body["options"]["destination"].as_str().unwrap();
                self.batches
                    .push((username, names, destination.to_string()));
                let batch = json!({"batch": {"id": format!("b{}", self.batches.len())},
                                   "failures": []});
                reply(201, &[], &batch.to_string())
            }
            ("GET", _) if path.starts_with("/transfers/downloads/") => {
                let user = path.strip_prefix("/transfers/downloads/").unwrap();
                let Some(transfers) = self.transfers.get(user) else {
                    return reply(404, &[], "User has no transfers");
                };
                let files: Vec<Value> = transfers
                    .iter()
                    .map(|t| {
                        json!({"id": t.id, "filename": t.filename, "size": t.size,
                               "state": t.state, "bytesTransferred": t.bytes})
                    })
                    .collect();
                let user = json!({"username": user, "directories": [{"files": files}]});
                reply(200, &[], &user.to_string())
            }
            ("DELETE", _) if path.starts_with("/transfers/downloads/") => {
                let rest = path.strip_prefix("/transfers/downloads/").unwrap();
                let (user, id) = rest.split_once('/').unwrap();
                let id = id.split('?').next().unwrap().to_string();
                if let Some(transfers) = self.transfers.get_mut(user) {
                    transfers.retain(|t| t.id != id);
                }
                self.cancelled.push(id);
                reply(204, &[], "")
            }
            _ => panic!("the made-up slskd has no {} {path}", request.method),
        }
    }
}

/// A test's made-up slskd, its folders and a store to look at.
struct Rig {
    slskd: Arc<Mutex<Slskd>>,
    client: Client,
    data_dir: PathBuf,
}

fn rig(test: &str) -> Rig {
    let dir = support::fresh_dir("download", test);
    let (_, data_dir) = support::write_config(&dir);
    let slskd = Arc::new(Mutex::new(Slskd::default()));
    let stub = {
        let slskd = Arc::clone(&slskd);
        Stub::start(move |request| slskd.lock().unwrap().answer(request))
    };
    let client = Client::new(&stub.url, "made-up-slskd-key");
    // The stub serves until the process ends; the rig needs only its URL.
    std::mem::forget(stub);
    Rig {
        slskd,
        client,
        data_dir,
    }
}

impl Rig {
    fn store(&self) -> Downloads {
        Downloads::open(&self.data_dir).unwrap()
    }

    fn job(&self, id: i64, clock: &FakeClock) -> catalog::download::Job {
        self.store().job(id, clock.seconds()).unwrap().unwrap()
    }

    fn runner<'a>(&'a self, clock: &'a FakeClock, log: &'a mut dyn FnMut(&str)) -> Runner<'a> {
        Runner::start(&self.data_dir, &self.client, clock, log).unwrap()
    }

    fn answer(&self, text: &str, responses: Vec<Value>) {
        self.slskd
            .lock()
            .unwrap()
            .responses
            .insert(text.to_string(), responses);
    }

    fn searched(&self) -> Vec<String> {
        self.slskd.lock().unwrap().searched.clone()
    }
}

/// The two-track release every test downloads.
fn new_job(release_id: i64) -> NewJob {
    NewJob {
        release: Release {
            release_id,
            artist: "Made-Up Artist".to_string(),
            title: "Made-Up Album".to_string(),
            year: Some(1999),
        },
        tracklists: vec![vec![
            Track {
                position: "1".to_string(),
                title: "First Song".to_string(),
                seconds: Some(245),
            },
            Track {
                position: "2".to_string(),
                title: "Second Song".to_string(),
                seconds: Some(312),
            },
        ]],
        format_policy: Policy::Flac,
        source: Source::Manual,
        upgrade: None,
    }
}

fn create(rig: &Rig, clock: &FakeClock) -> i64 {
    rig.store()
        .create(&new_job(4242), clock.seconds())
        .unwrap()
        .unwrap()
        .id
}

/// A peer's response with the release's two FLAC files under `FOLDER`.
fn flac_peer(username: &str) -> Value {
    json!({
        "username": username, "hasFreeUploadSlot": true, "queueLength": 0, "uploadSpeed": 1000,
        "files": [
            {"filename": format!("{FOLDER}\\01 - First Song.flac"), "size": 1000,
             "extension": "flac", "length": 245, "bitDepth": 16, "sampleRate": 44100},
            {"filename": format!("{FOLDER}\\02 - Second Song.flac"), "size": 2000,
             "extension": "flac", "length": 312, "bitDepth": 16, "sampleRate": 44100}
        ]
    })
}

/// The same files as MP3, which the `flac` policy rejects.
fn mp3_peer() -> Value {
    json!({
        "username": "mp3-peer", "hasFreeUploadSlot": true, "queueLength": 0, "uploadSpeed": 1000,
        "files": [
            {"filename": "@@m\\Made-Up Album\\01 - First Song.mp3", "size": 100,
             "extension": "mp3", "length": 245, "bitRate": 320},
            {"filename": "@@m\\Made-Up Album\\02 - Second Song.mp3", "size": 200,
             "extension": "mp3", "length": 312, "bitRate": 320}
        ]
    })
}

#[test]
fn a_queued_job_is_searched_and_its_best_candidate_chosen() {
    let rig = rig("search-found");
    let clock = FakeClock::new();
    let id = create(&rig, &clock);
    rig.answer(FIRST_QUERY, vec![mp3_peer(), flac_peer(PEER)]);
    let mut log = |_: &str| {};
    rig.runner(&clock, &mut log).tick().unwrap();
    let job = rig.job(id, &clock);
    assert_eq!(job.state, State::CandidateFound);
    assert_eq!(job.candidate.unwrap().peer, PEER);
    assert_eq!(job.files.len(), 2);
    // The first query found it, so the second wasn't sent.
    assert_eq!(rig.searched(), [FIRST_QUERY]);
}

#[test]
fn nothing_accepted_is_not_found_with_the_reason_after_both_queries_15_s_apart() {
    let rig = rig("search-not-found");
    let clock = FakeClock::new();
    let id = create(&rig, &clock);
    rig.answer(FIRST_QUERY, vec![mp3_peer()]);
    let mut log = |_: &str| {};
    rig.runner(&clock, &mut log).tick().unwrap();
    let job = rig.job(id, &clock);
    assert_eq!(job.state, State::NotFound);
    assert_eq!(job.reason.as_deref(), Some("only MP3 found"));
    assert_eq!(job.next_attempt_at, Some(clock.seconds() + 6 * HOUR));
    assert_eq!(rig.searched(), [FIRST_QUERY, "made up album artist"]);
    // One poll of the first search, then the rest of the 15 s gap before the second.
    assert_eq!(
        clock.sleeps(),
        [
            Duration::from_secs(1),
            Duration::from_secs(14),
            Duration::from_secs(1)
        ]
    );
}

#[test]
fn a_job_not_found_is_searched_again_on_the_schedule_until_the_8th_attempt() {
    let rig = rig("search-schedule");
    let clock = FakeClock::new();
    let id = create(&rig, &clock);
    let mut log = |_: &str| {};
    let mut runner = rig.runner(&clock, &mut log);
    let mut waits = Vec::new();
    loop {
        runner.tick().unwrap();
        let job = rig.job(id, &clock);
        assert_eq!(job.state, State::NotFound);
        let Some(at) = job.next_attempt_at else {
            assert_eq!(job.attempt, 8);
            break;
        };
        waits.push((at - clock.seconds()) / HOUR);
        // Not due a second early.
        clock.sleep(Duration::from_secs(
            (at - clock.seconds() - 1).unsigned_abs(),
        ));
        runner.tick().unwrap();
        assert_eq!(rig.job(id, &clock).state, State::NotFound);
        clock.sleep(Duration::from_secs(1));
    }
    assert_eq!(waits, [6, 24, 72, 168, 168, 168, 168]);
    // 8 attempts of 2 queries each.
    assert_eq!(rig.searched().len(), 16);
    // After the last, only a retry searches again.
    clock.sleep(Duration::from_hours(24 * 365));
    runner.tick().unwrap();
    assert_eq!(rig.searched().len(), 16);
}

#[test]
fn a_busy_slskd_is_retried_after_a_backoff() {
    let rig = rig("search-busy");
    let clock = FakeClock::new();
    let id = create(&rig, &clock);
    rig.answer(FIRST_QUERY, vec![flac_peer(PEER)]);
    rig.slskd.lock().unwrap().busy = 2;
    let mut log = |_: &str| {};
    rig.runner(&clock, &mut log).tick().unwrap();
    assert_eq!(rig.job(id, &clock).state, State::CandidateFound);
    assert_eq!(clock.sleeps()[..2], [Duration::from_secs(5); 2]);
}

#[test]
fn a_slskd_that_stays_busy_puts_the_job_back_as_the_same_attempt() {
    let rig = rig("search-busy-long");
    let clock = FakeClock::new();
    let id = create(&rig, &clock);
    rig.slskd.lock().unwrap().busy = 100;
    let mut log = |_: &str| {};
    let error = rig.runner(&clock, &mut log).tick().unwrap_err();
    assert!(matches!(error, RunError::Slskd(_)), "{error}");
    let job = rig.job(id, &clock);
    assert_eq!((job.state, job.attempt), (State::Queued, 1));
}

#[test]
fn a_search_left_running_by_a_restart_is_searched_again_as_the_same_attempt() {
    let rig = rig("search-restart");
    let clock = FakeClock::new();
    let id = create(&rig, &clock);
    rig.store()
        .transition(id, Event::Search, clock.seconds())
        .unwrap()
        .unwrap();
    rig.answer(FIRST_QUERY, vec![flac_peer(PEER)]);
    let mut log = |_: &str| {};
    let mut runner = rig.runner(&clock, &mut log);
    assert_eq!(rig.job(id, &clock).state, State::Queued);
    runner.tick().unwrap();
    let job = rig.job(id, &clock);
    assert_eq!((job.state, job.attempt), (State::CandidateFound, 1));
}

#[test]
fn a_second_runner_is_refused() {
    let rig = rig("second-runner");
    let clock = FakeClock::new();
    let mut log = |_: &str| {};
    let mut other_log = |_: &str| {};
    let _first = rig.runner(&clock, &mut log);
    let error = Runner::start(&rig.data_dir, &rig.client, &clock, &mut other_log)
        .err()
        .unwrap();
    assert!(matches!(error, RunError::Busy(_)), "{error}");
    assert!(error.to_string().contains("downloads.lock"), "{error}");
}

#[test]
fn a_reply_slskd_does_not_send_fails_the_search() {
    let rig = rig("search-bad-reply");
    let clock = FakeClock::new();
    let id = create(&rig, &clock);
    rig.answer(FIRST_QUERY, vec![json!({"username": PEER})]);
    let mut log = |_: &str| {};
    rig.runner(&clock, &mut log).tick().unwrap();
    let job = rig.job(id, &clock);
    assert_eq!(job.state, State::Failed(Stage::Search));
    assert!(job.reason.unwrap().starts_with("slskd's search reply"));
}
