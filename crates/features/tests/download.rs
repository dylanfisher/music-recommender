//! The download runner against a made-up slskd on the stub server, with a made-up clock and
//! temp folders: nothing waits on real time, and nothing reaches a real slskd or the NAS.

#[path = "support/clock.rs"]
mod clock;
#[path = "../../sources/tests/support/stub.rs"]
mod stub;
#[path = "../../config/tests/support/setup.rs"]
mod support;

use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use catalog::download::{
    Downloads, Event, Handoff, NewJob, Policy, Release, Source, Stage, State, Tagging,
    TaggingStatus, Track,
};
use clock::FakeClock;
use config::Config;
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

/// Plenty of room, as the free-space reading every test but one injects.
#[expect(clippy::unnecessary_wraps, reason = "the runner's free-space reading")]
fn roomy(_: &Path) -> io::Result<u64> {
    Ok(1 << 40)
}

/// A test's made-up slskd, its config and folders, and a store to look at.
struct Rig {
    slskd: Arc<Mutex<Slskd>>,
    client: Client,
    config: Config,
    downloads_dir: PathBuf,
    staging_dir: PathBuf,
}

fn rig(test: &str) -> Rig {
    let dir = support::fresh_dir("download", test);
    let (path, _) = support::write_config(&dir);
    let downloads_dir = dir.join("slskd-downloads");
    let staging_dir = dir.join("staging");
    fs::create_dir_all(&downloads_dir).unwrap();
    fs::create_dir_all(&staging_dir).unwrap();
    let slskd = Arc::new(Mutex::new(Slskd::default()));
    let stub = {
        let slskd = Arc::clone(&slskd);
        Stub::start(move |request| slskd.lock().unwrap().answer(request))
    };
    let tables = format!(
        "\n[slskd]\nurl = \"{}\"\napi_key = \"made-up-slskd-key\"\ndownloads_dir = \"{}\"\n\
         [staging]\ndir = \"{}\"\n",
        stub.url,
        downloads_dir.display(),
        staging_dir.display()
    );
    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(tables.as_bytes()).unwrap();
    let config = config::load(&path).unwrap();
    let settings = config.downloads.as_ref().unwrap();
    let client = Client::new(&settings.slskd_url, settings.slskd_api_key.secret());
    // The stub serves until the process ends; the rig needs only its URL.
    std::mem::forget(stub);
    Rig {
        slskd,
        client,
        config,
        downloads_dir: fs::canonicalize(downloads_dir).unwrap(),
        staging_dir: fs::canonicalize(staging_dir).unwrap(),
    }
}

impl Rig {
    fn store(&self) -> Downloads {
        Downloads::open(&self.config.data_dir).unwrap()
    }

    fn job(&self, id: i64, clock: &FakeClock) -> catalog::download::Job {
        self.store().job(id, clock.seconds()).unwrap().unwrap()
    }

    fn runner<'a>(&'a self, clock: &'a FakeClock, log: &'a mut dyn FnMut(&str)) -> Runner<'a> {
        Runner::start(&self.config, &self.client, clock, &roomy, log).unwrap()
    }

    /// The made-up slskd's transfers from `user`.
    fn transfers(&self, user: &str) -> Vec<Transfer> {
        let slskd = self.slskd.lock().unwrap();
        slskd.transfers.get(user).cloned().unwrap_or_default()
    }

    /// Sets each of `user`'s transfers that `pick` names to `state`, with `bytes`.
    fn set(&self, user: &str, pick: impl Fn(&Transfer) -> bool, state: &str, bytes: u64) {
        let mut slskd = self.slskd.lock().unwrap();
        for transfer in slskd.transfers.get_mut(user).unwrap() {
            if pick(transfer) {
                transfer.state = state.to_string();
                transfer.bytes = bytes;
            }
        }
    }

    /// Finishes each of `user`'s transfers as slskd does: the file written into `folder`
    /// under the downloads folder, and the transfer succeeded.
    fn finish(&self, user: &str, folder: &str) {
        for transfer in self.transfers(user) {
            let name = transfer.filename.rsplit('\\').next().unwrap();
            let dir = self.downloads_dir.join(folder);
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join(name),
                vec![0; usize::try_from(transfer.size).unwrap()],
            )
            .unwrap();
        }
        self.set(user, |_| true, "Completed, Succeeded", 0);
    }

    fn batches(&self) -> Vec<(String, Vec<String>, String)> {
        self.slskd.lock().unwrap().batches.clone()
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
    create_release(rig, clock, 4242)
}

fn create_release(rig: &Rig, clock: &FakeClock, release_id: i64) -> i64 {
    rig.store()
        .create(&new_job(release_id), clock.seconds())
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
    let error = Runner::start(&rig.config, &rig.client, &clock, &roomy, &mut other_log)
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

/// Creates a job, has slskd answer the first query with `peers`, and runs the rounds that
/// search for it and enqueue its first candidate.
fn downloading(rig: &Rig, clock: &FakeClock, peers: Vec<Value>) -> i64 {
    let id = create(rig, clock);
    rig.answer(FIRST_QUERY, peers);
    let mut log = |_: &str| {};
    let mut runner = rig.runner(clock, &mut log);
    runner.tick().unwrap();
    runner.tick().unwrap();
    assert_eq!(rig.job(id, clock).state, State::Downloading);
    id
}

fn tick(rig: &Rig, clock: &FakeClock) {
    let mut log = |_: &str| {};
    rig.runner(clock, &mut log).tick().unwrap();
}

#[test]
fn a_candidate_is_enqueued_into_the_job_s_folder_and_downloaded() {
    let rig = rig("download");
    let clock = FakeClock::new();
    let id = downloading(&rig, &clock, vec![flac_peer(PEER)]);
    let batches = rig.batches();
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].0, PEER);
    assert_eq!(batches[0].2, format!("job-{id}"));
    assert_eq!(batches[0].1.len(), 2);

    rig.set(
        PEER,
        |t| t.filename.ends_with("01 - First Song.flac"),
        "InProgress",
        400,
    );
    tick(&rig, &clock);
    let progress = rig.job(id, &clock).progress.unwrap();
    assert_eq!(
        (
            progress.files_done,
            progress.bytes_done,
            progress.bytes_total
        ),
        (0, 400, 3000)
    );

    rig.finish(PEER, &format!("job-{id}"));
    tick(&rig, &clock);
    assert_eq!(rig.job(id, &clock).state, State::Downloaded);
    let folder = rig.downloads_dir.join(format!("job-{id}"));
    assert_eq!(
        rig.store().ledger(id).unwrap(),
        [
            folder.join("01 - First Song.flac").display().to_string(),
            folder.join("02 - Second Song.flac").display().to_string()
        ]
    );
    // Enqueued once, whatever the rounds.
    assert_eq!(rig.batches().len(), 1);
}

#[test]
fn files_slskd_put_in_the_peer_s_folder_are_found_there() {
    let rig = rig("download-own-folder");
    let clock = FakeClock::new();
    let id = downloading(&rig, &clock, vec![flac_peer(PEER)]);
    rig.finish(PEER, "Made-Up Album (1999) [FLAC]");
    tick(&rig, &clock);
    assert_eq!(rig.job(id, &clock).state, State::Downloaded);
}

#[test]
fn a_restart_between_the_request_and_its_record_adopts_the_transfers() {
    let rig = rig("download-adopt");
    let clock = FakeClock::new();
    let id = create(&rig, &clock);
    rig.answer(FIRST_QUERY, vec![flac_peer(PEER)]);
    tick(&rig, &clock);
    assert_eq!(rig.job(id, &clock).state, State::CandidateFound);
    // The runner sent the batch and stopped before recording it.
    let candidate = rig.store().chosen(id).unwrap().unwrap();
    let files: Vec<Value> = candidate
        .files
        .iter()
        .map(|f| json!({"filename": f.remote, "size": f.size}))
        .collect();
    let body = json!({"username": PEER, "files": files, "options": {"destination": "job-1"}});
    let request = stub::Request {
        method: "POST".to_string(),
        path: "/api/v0/transfers/downloads/batches".to_string(),
        headers: Vec::new(),
        body: body.to_string().into_bytes(),
    };
    rig.slskd.lock().unwrap().answer(&request);
    tick(&rig, &clock);
    assert_eq!(rig.job(id, &clock).state, State::Downloading);
    assert_eq!(rig.batches().len(), 1, "enqueued again");
}

#[test]
fn no_file_complete_in_24_hours_falls_to_the_next_candidate() {
    let rig = rig("download-24h");
    let clock = FakeClock::new();
    let id = downloading(&rig, &clock, vec![flac_peer("peer-a"), flac_peer("peer-b")]);
    let first = rig.store().chosen(id).unwrap().unwrap().peer;
    clock.sleep(Duration::from_hours(24) - Duration::from_secs(1));
    tick(&rig, &clock);
    assert_eq!(rig.store().chosen(id).unwrap().unwrap().peer, first);
    clock.sleep(Duration::from_secs(1));
    tick(&rig, &clock);
    // The first peer's transfers are cancelled, the next one's enqueued.
    assert_eq!(rig.transfers(&first).len(), 0);
    let second = rig.store().chosen(id).unwrap().unwrap().peer;
    assert_ne!(second, first);
    tick(&rig, &clock);
    assert_eq!(rig.job(id, &clock).state, State::Downloading);
    assert_eq!(rig.batches().last().unwrap().0, second);
}

#[test]
fn a_file_gaining_nothing_for_6_hours_falls_to_the_next_candidate() {
    let rig = rig("download-6h");
    let clock = FakeClock::new();
    let id = downloading(&rig, &clock, vec![flac_peer("peer-a"), flac_peer("peer-b")]);
    let first = rig.store().chosen(id).unwrap().unwrap().peer;
    rig.finish(&first, &format!("job-{id}"));
    // Only the first is complete: slskd writes a file in progress elsewhere.
    fs::remove_file(
        rig.downloads_dir
            .join(format!("job-{id}/02 - Second Song.flac")),
    )
    .unwrap();
    rig.set(
        &first,
        |t| t.filename.ends_with("02 - Second Song.flac"),
        "InProgress",
        10,
    );
    let mut log = |_: &str| {};
    let mut runner = rig.runner(&clock, &mut log);
    runner.tick().unwrap();
    clock.sleep(Duration::from_hours(5));
    rig.set(
        &first,
        |t| t.filename.ends_with("02 - Second Song.flac"),
        "InProgress",
        20,
    );
    runner.tick().unwrap();
    clock.sleep(Duration::from_hours(6) - Duration::from_secs(1));
    runner.tick().unwrap();
    assert_eq!(rig.store().chosen(id).unwrap().unwrap().peer, first);
    clock.sleep(Duration::from_secs(1));
    runner.tick().unwrap();
    assert_ne!(rig.store().chosen(id).unwrap().unwrap().peer, first);
    // The first candidate's finished file is deleted with its ledger entry.
    assert_eq!(rig.store().ledger(id).unwrap(), Vec::<String>::new());
    assert!(!rig.downloads_dir.join(format!("job-{id}")).exists());
}

#[test]
fn a_file_failing_3_times_falls_to_the_next_candidate_and_then_searches_again() {
    let rig = rig("download-errors");
    let clock = FakeClock::new();
    let id = downloading(&rig, &clock, vec![flac_peer(PEER)]);
    let first_song = |t: &Transfer| t.filename.ends_with("01 - First Song.flac");
    let mut log = |_: &str| {};
    let mut runner = rig.runner(&clock, &mut log);
    for _ in 0..2 {
        rig.set(PEER, first_song, "Completed, Errored", 0);
        runner.tick().unwrap();
        // Enqueued again, the failed transfer's record removed.
        assert_eq!(rig.job(id, &clock).state, State::Downloading);
        assert_eq!(rig.transfers(PEER).len(), 2);
    }
    assert_eq!(rig.batches().len(), 3);
    rig.set(PEER, first_song, "Completed, Rejected", 0);
    runner.tick().unwrap();
    // The only candidate failed: a new attempt, searched in the same round.
    let job = rig.job(id, &clock);
    assert_eq!((job.state, job.attempt), (State::CandidateFound, 2));
    assert_eq!(rig.transfers(PEER).len(), 0);
    assert_eq!(rig.searched(), [FIRST_QUERY, FIRST_QUERY]);
}

#[test]
fn cancelling_stops_the_transfers_and_deletes_only_the_ledgered_files() {
    let rig = rig("download-cancel");
    let clock = FakeClock::new();
    let id = downloading(&rig, &clock, vec![flac_peer(PEER)]);
    rig.finish(PEER, &format!("job-{id}"));
    rig.set(
        PEER,
        |t| t.filename.ends_with("02 - Second Song.flac"),
        "InProgress",
        5,
    );
    // The owner's own download, beside the job's folder and in it.
    fs::write(rig.downloads_dir.join("owner.flac"), "mine").unwrap();
    tick(&rig, &clock);
    fs::write(
        rig.downloads_dir.join(format!("job-{id}/owner.flac")),
        "mine",
    )
    .unwrap();
    rig.store()
        .transition(id, Event::Cancel, clock.seconds())
        .unwrap()
        .unwrap();
    tick(&rig, &clock);
    let job = rig.job(id, &clock);
    assert_eq!(job.state, State::Cancelled);
    assert!(!job.cleanup_pending);
    assert_eq!(rig.transfers(PEER).len(), 0);
    let folder = rig.downloads_dir.join(format!("job-{id}"));
    assert!(!folder.join("01 - First Song.flac").exists());
    assert!(folder.join("owner.flac").exists());
    assert!(rig.downloads_dir.join("owner.flac").exists());
}

#[test]
fn at_most_4_jobs_download_at_once() {
    let rig = rig("download-limit");
    let clock = FakeClock::new();
    rig.answer(FIRST_QUERY, vec![flac_peer(PEER)]);
    let ids: Vec<i64> = (1..=5).map(|r| create_release(&rig, &clock, r)).collect();
    let mut log = |_: &str| {};
    let mut runner = rig.runner(&clock, &mut log);
    for _ in 0..6 {
        runner.tick().unwrap();
    }
    let states: Vec<State> = ids.iter().map(|&id| rig.job(id, &clock).state).collect();
    assert_eq!(
        states.iter().filter(|s| **s == State::Downloading).count(),
        4
    );
    assert_eq!(states[4], State::CandidateFound);
}

#[test]
fn an_album_over_the_size_cap_is_never_enqueued() {
    let rig = rig("download-too-large");
    let clock = FakeClock::new();
    let id = create(&rig, &clock);
    let mut peer = flac_peer(PEER);
    peer["files"][1]["size"] = json!(2049_u64 << 20);
    rig.answer(FIRST_QUERY, vec![peer]);
    tick(&rig, &clock);
    tick(&rig, &clock);
    assert_eq!(rig.batches().len(), 0);
    // Its only candidate fell through, and the next attempt was searched for.
    let job = rig.job(id, &clock);
    assert_eq!((job.state, job.attempt), (State::CandidateFound, 2));
}

#[test]
fn too_little_room_for_the_album_fails_its_download() {
    let rig = rig("download-no-room");
    let clock = FakeClock::new();
    let id = create(&rig, &clock);
    rig.answer(FIRST_QUERY, vec![flac_peer(PEER)]);
    let full = |_: &Path| -> io::Result<u64> { Ok(2999) };
    let mut log = |_: &str| {};
    let mut runner = Runner::start(&rig.config, &rig.client, &clock, &full, &mut log).unwrap();
    runner.tick().unwrap();
    runner.tick().unwrap();
    let job = rig.job(id, &clock);
    assert_eq!(job.state, State::Failed(Stage::Download));
    assert!(job.reason.unwrap().contains("needs 1 MB"));
    assert_eq!(rig.batches().len(), 0);
}

#[test]
fn serve_searches_and_enqueues_on_its_own_rounds() {
    let rig = rig("download-serve");
    let mut clock = FakeClock::new();
    let id = create(&rig, &clock);
    rig.answer(FIRST_QUERY, vec![flac_peer(PEER)]);
    clock.on_sleep = Box::new(|sleeps| assert!(sleeps < 20, "{STOP}"));
    let stopped = panic::catch_unwind(AssertUnwindSafe(|| {
        features::download::serve(&rig.config, &rig.client, &clock, &roomy, &mut |_| {})
    }))
    .unwrap_err();
    assert!(stopped.downcast_ref::<String>().unwrap().contains(STOP));
    assert_eq!(rig.job(id, &clock).state, State::Downloading);
    assert!(clock.sleeps().contains(&Duration::from_secs(5)));
}

/// The panic message that ends a test's `serve`, which never returns.
const STOP: &str = "stop serve";

#[test]
fn every_candidate_failing_in_the_8th_attempt_fails_the_download() {
    let rig = rig("download-last-attempt");
    let clock = FakeClock::new();
    let id = downloading(&rig, &clock, vec![flac_peer(PEER)]);
    let first_song = |t: &Transfer| t.filename.ends_with("01 - First Song.flac");
    let mut log = |_: &str| {};
    let mut runner = rig.runner(&clock, &mut log);
    for attempt in 1..=8 {
        assert_eq!(rig.job(id, &clock).attempt, attempt);
        for _ in 0..3 {
            rig.set(PEER, first_song, "Completed, Errored", 0);
            runner.tick().unwrap();
        }
        if attempt < 8 {
            runner.tick().unwrap();
            assert_eq!(rig.job(id, &clock).state, State::Downloading);
        }
    }
    let job = rig.job(id, &clock);
    assert_eq!(job.state, State::Failed(Stage::Download));
    assert!(job.reason.unwrap().starts_with("every candidate failed"));
}

/// Runs a job to `downloaded`, its files in the job's folder under slskd's.
fn downloaded(rig: &Rig, clock: &FakeClock, peer: Value) -> i64 {
    let id = downloading(rig, clock, vec![peer]);
    rig.finish(PEER, &format!("job-{id}"));
    tick(rig, clock);
    assert_eq!(rig.job(id, clock).state, State::Downloaded);
    id
}

#[test]
fn a_downloaded_job_is_copied_into_staging_and_slskd_s_copies_deleted() {
    let rig = rig("stage");
    let clock = FakeClock::new();
    let id = downloaded(&rig, &clock, flac_peer(PEER));
    fs::write(rig.downloads_dir.join("owner.flac"), "mine").unwrap();
    tick(&rig, &clock);
    let job = rig.job(id, &clock);
    assert_eq!(job.state, State::Staged);
    let folder = format!("job-{id}");
    assert_eq!(job.staged_path.as_deref(), Some(folder.as_str()));
    let names: Vec<(&str, u64)> = job
        .files
        .iter()
        .map(|f| (f.name.as_str(), f.size))
        .collect();
    assert_eq!(
        names,
        [
            ("01 - First Song.flac", 1000),
            ("02 - Second Song.flac", 2000)
        ]
    );
    for file in &job.files {
        let staged = rig.staging_dir.join(&folder).join(&file.name);
        assert_eq!(fs::metadata(staged).unwrap().len(), file.size);
    }
    assert!(!rig.downloads_dir.join(&folder).exists());
    assert!(rig.downloads_dir.join("owner.flac").exists());
    assert_eq!(rig.store().ledger(id).unwrap(), Vec::<String>::new());
    assert!(!rig.staging_dir.join(".incoming").join(&folder).exists());
}

#[test]
fn slskd_s_ledgered_copies_of_a_staged_job_are_deleted_on_a_later_round() {
    let rig = rig("stage-ledger-retry");
    let clock = FakeClock::new();
    let id = staged(&rig, &clock);
    for (round, handoff) in [None, Some(Handoff::Approved), Some(Handoff::Imported)]
        .into_iter()
        .enumerate()
    {
        if let Some(handoff) = handoff {
            report(&rig, &clock, id, &handoff);
        }
        // The delete after `staged()` failed or the runner died there.
        let left = rig.downloads_dir.join(format!("job-{id}-{round}.flac"));
        fs::write(&left, "left").unwrap();
        rig.store()
            .ledger_add(id, &left.display().to_string())
            .unwrap();
        tick(&rig, &clock);
        assert!(!left.exists(), "round {round}");
        assert_eq!(rig.store().ledger(id).unwrap(), Vec::<String>::new());
    }
}

#[test]
fn a_hostile_file_name_is_staged_as_a_plain_one() {
    let rig = rig("stage-hostile");
    let clock = FakeClock::new();
    let mut peer = flac_peer(PEER);
    peer["files"][0]["filename"] = json!(format!("{FOLDER}\\01 - First: Song\u{7}.flac"));
    let id = downloaded(&rig, &clock, peer);
    tick(&rig, &clock);
    let job = rig.job(id, &clock);
    assert_eq!(job.state, State::Staged);
    assert_eq!(job.files[0].name, "01 - First_ Song.flac");
    assert!(
        rig.staging_dir
            .join(format!("job-{id}/01 - First_ Song.flac"))
            .exists()
    );
}

#[test]
fn a_copy_stopped_before_its_rename_is_cleared_at_start_and_made_again() {
    let rig = rig("stage-crash");
    let clock = FakeClock::new();
    let id = downloaded(&rig, &clock, flac_peer(PEER));
    let incoming = rig.staging_dir.join(format!(".incoming/job-{id}"));
    fs::create_dir_all(&incoming).unwrap();
    fs::write(incoming.join("01 - First Song.flac"), "half").unwrap();
    // A copy renamed into place whose record never got written.
    let placed = rig.staging_dir.join(format!("job-{id}"));
    fs::create_dir_all(&placed).unwrap();
    fs::write(placed.join("stray.flac"), "x").unwrap();
    let mut log = |_: &str| {};
    let mut runner = rig.runner(&clock, &mut log);
    assert!(!rig.staging_dir.join(".incoming").exists());
    runner.tick().unwrap();
    assert_eq!(rig.job(id, &clock).state, State::Staged);
    assert!(!placed.join("stray.flac").exists());
}

#[test]
fn a_file_changed_in_slskd_s_folder_fails_the_stage_until_retried() {
    let rig = rig("stage-size");
    let clock = FakeClock::new();
    let id = downloaded(&rig, &clock, flac_peer(PEER));
    let file = rig
        .downloads_dir
        .join(format!("job-{id}/02 - Second Song.flac"));
    fs::write(&file, "short").unwrap();
    tick(&rig, &clock);
    let job = rig.job(id, &clock);
    assert_eq!(job.state, State::Failed(Stage::Stage));
    assert!(job.reason.unwrap().contains("02 - Second Song.flac"));
    assert!(!rig.staging_dir.join(format!("job-{id}")).exists());
    fs::write(&file, vec![0; 2000]).unwrap();
    rig.store()
        .retry(id, None, clock.seconds())
        .unwrap()
        .unwrap();
    tick(&rig, &clock);
    assert_eq!(rig.job(id, &clock).state, State::Staged);
}

#[test]
fn an_unmounted_staging_folder_fails_the_stage_naming_it() {
    let rig = rig("stage-unmounted");
    let clock = FakeClock::new();
    let id = downloaded(&rig, &clock, flac_peer(PEER));
    fs::remove_dir_all(&rig.staging_dir).unwrap();
    tick(&rig, &clock);
    let job = rig.job(id, &clock);
    assert_eq!(job.state, State::Failed(Stage::Stage));
    assert!(job.reason.unwrap().contains("is the NAS mounted?"));
    // slskd's copies stay for the retry.
    assert!(rig.downloads_dir.join(format!("job-{id}")).exists());
}

#[test]
fn too_little_room_in_staging_fails_the_stage() {
    let rig = rig("stage-no-room");
    let clock = FakeClock::new();
    let id = downloaded(&rig, &clock, flac_peer(PEER));
    let staging = rig.staging_dir.clone();
    let space =
        move |path: &Path| -> io::Result<u64> { Ok(if path == staging { 10 } else { 1 << 40 }) };
    let mut log = |_: &str| {};
    Runner::start(&rig.config, &rig.client, &clock, &space, &mut log)
        .unwrap()
        .tick()
        .unwrap();
    let job = rig.job(id, &clock);
    assert_eq!(job.state, State::Failed(Stage::Stage));
    assert!(job.reason.unwrap().contains("free in the staging folder"));
}

/// Runs a job to `staged` and has the player hold the review claim on it.
fn staged(rig: &Rig, clock: &FakeClock) -> i64 {
    let id = downloaded(rig, clock, flac_peer(PEER));
    tick(rig, clock);
    let mut store = rig.store();
    let now = clock.seconds();
    for handoff in [
        Handoff::Lease,
        Handoff::Tagging(Tagging {
            status: TaggingStatus::Clean,
            reasons: Vec::new(),
        }),
        Handoff::Claim { take_over: false },
    ] {
        store
            .handoff(id, "Living Room", &handoff, now)
            .unwrap()
            .unwrap();
    }
    id
}

fn report(rig: &Rig, clock: &FakeClock, id: i64, handoff: &Handoff) {
    rig.store()
        .handoff(id, "Living Room", handoff, clock.seconds())
        .unwrap()
        .unwrap();
}

#[test]
fn a_rejected_job_s_staged_files_are_deleted_and_nothing_else() {
    let rig = rig("reject");
    let clock = FakeClock::new();
    let id = staged(&rig, &clock);
    let folder = rig.staging_dir.join(format!("job-{id}"));
    fs::write(folder.join(".DS_Store"), "finder").unwrap();
    report(&rig, &clock, id, &Handoff::Rejected);
    tick(&rig, &clock);
    // The ledgered files are gone; a file the job didn't stage keeps the folder and the flag.
    assert!(!folder.join("01 - First Song.flac").exists());
    assert!(folder.join(".DS_Store").exists());
    assert!(rig.job(id, &clock).cleanup_pending);
    fs::remove_file(folder.join(".DS_Store")).unwrap();
    tick(&rig, &clock);
    let job = rig.job(id, &clock);
    assert_eq!(job.state, State::Rejected);
    assert!(!job.cleanup_pending);
    assert!(!folder.exists());
}

#[test]
fn cleanup_that_fails_is_tried_again_in_later_rounds() {
    let rig = rig("cleanup-retry");
    let clock = FakeClock::new();
    let id = staged(&rig, &clock);
    let folder = rig.staging_dir.join(format!("job-{id}"));
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o500)).unwrap();
    report(&rig, &clock, id, &Handoff::Rejected);
    let mut lines = Vec::new();
    {
        let mut log = |line: &str| lines.push(line.to_string());
        let mut runner = rig.runner(&clock, &mut log);
        runner.tick().unwrap();
        runner.tick().unwrap();
    }
    assert!(rig.job(id, &clock).cleanup_pending);
    assert_eq!(
        lines.iter().filter(|l| l.contains("can't")).count(),
        1,
        "{lines:?}"
    );
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o700)).unwrap();
    tick(&rig, &clock);
    assert!(!rig.job(id, &clock).cleanup_pending);
    assert!(!folder.exists());
}

#[test]
fn an_imported_job_s_folder_is_removed_once_empty_and_its_files_never_deleted() {
    let rig = rig("imported");
    let clock = FakeClock::new();
    let id = staged(&rig, &clock);
    report(&rig, &clock, id, &Handoff::Approved);
    report(&rig, &clock, id, &Handoff::Imported);
    let folder = rig.staging_dir.join(format!("job-{id}"));
    // The player moved one file and not yet the other.
    fs::remove_file(folder.join("01 - First Song.flac")).unwrap();
    tick(&rig, &clock);
    assert!(folder.join("02 - Second Song.flac").exists());
    fs::remove_file(folder.join("02 - Second Song.flac")).unwrap();
    tick(&rig, &clock);
    assert!(!folder.exists());
}

#[test]
fn a_cancelled_staged_job_s_folder_is_deleted() {
    let rig = rig("cancel-staged");
    let clock = FakeClock::new();
    let id = staged(&rig, &clock);
    rig.store()
        .transition(id, Event::Cancel, clock.seconds())
        .unwrap()
        .unwrap();
    tick(&rig, &clock);
    assert!(!rig.job(id, &clock).cleanup_pending);
    assert!(!rig.staging_dir.join(format!("job-{id}")).exists());
}

#[test]
fn a_cancel_during_the_copy_leaves_no_staging_folder() {
    let rig = rig("cancel-copying");
    let clock = FakeClock::new();
    let id = downloaded(&rig, &clock, flac_peer(PEER));
    // The owner cancels after the runner read the job and before its copy is recorded.
    let data_dir = rig.config.data_dir.clone();
    let seconds = clock.seconds();
    let cancel = move |_: &Path| {
        Downloads::open(&data_dir)
            .unwrap()
            .transition(id, Event::Cancel, seconds)
            .unwrap()
            .unwrap();
        Ok(1 << 40)
    };
    let mut log = |_: &str| {};
    Runner::start(&rig.config, &rig.client, &clock, &cancel, &mut log)
        .unwrap()
        .tick()
        .unwrap();
    tick(&rig, &clock);
    assert!(!rig.job(id, &clock).cleanup_pending);
    assert!(!rig.staging_dir.join(format!("job-{id}")).exists());
    assert!(
        !rig.staging_dir
            .join(".incoming")
            .join(format!("job-{id}"))
            .exists()
    );
}

#[test]
fn a_terminal_job_is_purged_30_days_after_it_ended() {
    let rig = rig("purge");
    let clock = FakeClock::new();
    let id = create(&rig, &clock);
    rig.store()
        .transition(id, Event::Cancel, clock.seconds())
        .unwrap()
        .unwrap();
    clock.sleep(Duration::from_hours(30 * 24) - Duration::from_secs(1));
    tick(&rig, &clock);
    assert!(rig.store().job(id, clock.seconds()).unwrap().is_some());
    clock.sleep(Duration::from_secs(1));
    tick(&rig, &clock);
    assert_eq!(rig.store().job(id, clock.seconds()).unwrap(), None);
}
