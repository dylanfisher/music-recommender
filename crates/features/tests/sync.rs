//! The sync loop against the stub Discogs API, with a made-up clock: nothing waits on
//! real time.

#[path = "../../sources/tests/support/stub.rs"]
mod stub;

use std::cell::{Cell, RefCell};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

use catalog::community::{Community, Source};
use config::Config;
use features::clock::Clock;
use features::sync::{self, Stopped, Summary, SyncError};
use stub::{Stub, reply};

/// The made-up clock's start, in Unix seconds.
const START: i64 = 1_800_000_000;

/// A clock that starts at a fixed time and moves only when slept on.
struct FakeClock {
    now: Cell<SystemTime>,
    sleeps: RefCell<Vec<Duration>>,
}

impl FakeClock {
    fn new() -> Self {
        Self {
            now: Cell::new(SystemTime::UNIX_EPOCH + Duration::from_secs(START.unsigned_abs())),
            sleeps: RefCell::new(Vec::new()),
        }
    }
}

impl Clock for FakeClock {
    fn now(&self) -> SystemTime {
        self.now.get()
    }

    fn sleep(&self, duration: Duration) {
        self.sleeps.borrow_mut().push(duration);
        self.now.set(self.now.get() + duration);
    }
}

/// A config in a fresh directory named for the test, pointing at `stub`, with `ids` queued
/// as the library's.
fn setup(test: &str, stub: &Stub, ids: &[u64]) -> Config {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("sync")
        .join(test);
    let _ = fs::remove_dir_all(&dir);
    let data_dir = dir.join("data");
    fs::create_dir_all(&data_dir).unwrap();
    let path = dir.join("config.toml");
    fs::write(
        &path,
        format!(
            "discogs_token = \"made-up-token\"\ndata_dir = \"{}\"\ndiscogs_api_url = \"{}\"\n",
            data_dir.display(),
            stub.url
        ),
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let config = config::load(&path).unwrap();
    Community::open(&config.data_dir)
        .unwrap()
        .queue(ids, Source::Library)
        .unwrap();
    config
}

fn run(config: &Config, clock: &FakeClock, limit: Option<u64>) -> Result<Summary, SyncError> {
    sync::run(config, clock, limit, &mut |_| {})
}

fn requested(stub: &Stub) -> Vec<u64> {
    stub.received()
        .iter()
        .map(stub::Request::release_id)
        .collect()
}

fn progress(config: &Config) -> catalog::community::Progress {
    Community::open(&config.data_dir)
        .unwrap()
        .progress(START)
        .unwrap()
}

#[test]
fn a_run_stopped_after_a_reply_continues_with_the_next_id() {
    let stub = Stub::releases();
    let config = setup("restart", &stub, &[11, 12, 13]);
    let clock = FakeClock::new();
    assert_eq!(
        run(&config, &clock, Some(1)).unwrap(),
        Summary {
            requests: 1,
            stopped: Stopped::Limit
        }
    );
    assert_eq!(requested(&stub), [11]);
    assert_eq!(
        run(&config, &clock, None).unwrap(),
        Summary {
            requests: 2,
            stopped: Stopped::Done
        }
    );
    assert_eq!(requested(&stub), [12, 13]);
    assert_eq!(progress(&config).fetched, 3);
    assert!(clock.sleeps.borrow().is_empty());
}

#[test]
fn pausing_stops_after_the_reply_in_flight() {
    let data_dir = Arc::new(Mutex::new(PathBuf::new()));
    let stub = {
        let data_dir = Arc::clone(&data_dir);
        // Another process pausing while request 21 is in flight.
        Stub::start(move |request| {
            if request.release_id() == 21 {
                let dir = data_dir.lock().unwrap();
                Community::open(&dir).unwrap().set_paused(true).unwrap();
            }
            reply(200, &[], stub::RELEASE_JSON)
        })
    };
    let config = setup("pause", &stub, &[21, 22]);
    data_dir.lock().unwrap().clone_from(&config.data_dir);
    let clock = FakeClock::new();
    assert_eq!(run(&config, &clock, None).unwrap().stopped, Stopped::Paused);
    assert_eq!(requested(&stub), [21]);
    let progress = progress(&config);
    assert_eq!((progress.fetched, progress.queued), (1, 1));

    // Paused, a new run sends nothing until resumed.
    assert_eq!(run(&config, &clock, None).unwrap().requests, 0);
    sync::set_paused(&config, false).unwrap();
    assert_eq!(run(&config, &clock, None).unwrap().requests, 1);
    assert_eq!(requested(&stub), [22]);
}

#[test]
fn a_second_sync_refuses_to_start() {
    let (arrived, wait_for_arrival) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let released = Mutex::new(released);
    let stub = Stub::start(move |_| {
        arrived.send(()).unwrap();
        released.lock().unwrap().recv().unwrap();
        reply(200, &[], stub::RELEASE_JSON)
    });
    let config = Arc::new(setup("second", &stub, &[31]));
    let first = {
        let config = Arc::clone(&config);
        thread::spawn(move || run(&config, &FakeClock::new(), None))
    };
    wait_for_arrival
        .recv_timeout(Duration::from_secs(30))
        .unwrap();
    let error = run(&config, &FakeClock::new(), None).unwrap_err();
    assert!(matches!(error, SyncError::Busy(_)), "{error}");
    assert!(error.to_string().contains("sync.lock"), "{error}");
    release.send(()).unwrap();
    assert_eq!(first.join().unwrap().unwrap().requests, 1);
    // The lock is free once the first run ends.
    assert_eq!(run(&config, &FakeClock::new(), None).unwrap().requests, 0);
}

#[test]
fn a_refused_token_stops_the_sync_naming_the_config_file() {
    let stub = Stub::start(|_| reply(401, &[], "{\"message\": \"You must authenticate.\"}"));
    let config = setup("refused", &stub, &[41, 42]);
    let error = run(&config, &FakeClock::new(), None).unwrap_err();
    assert!(matches!(error, SyncError::TokenRefused(_, 401)), "{error}");
    let message = error.to_string();
    assert!(
        message.contains(&config.path.display().to_string()),
        "{message}"
    );
    assert!(!message.contains("made-up-token"), "{message}");
    assert_eq!(requested(&stub), [41]);
    let progress = progress(&config);
    assert_eq!(progress.queued, 2);
    assert!(progress.last_error.unwrap().1.contains("refused"));
}

#[test]
fn a_429_waits_what_it_says_and_retries_the_same_release() {
    let calls = Mutex::new(0);
    let stub = Stub::start(move |_| {
        let mut calls = calls.lock().unwrap();
        *calls += 1;
        if *calls == 1 {
            reply(429, &[("Retry-After", "7")], "{}")
        } else {
            reply(200, &[], stub::RELEASE_JSON)
        }
    });
    let config = setup("429", &stub, &[51]);
    let clock = FakeClock::new();
    assert_eq!(run(&config, &clock, None).unwrap().requests, 2);
    assert_eq!(requested(&stub), [51, 51]);
    assert_eq!(*clock.sleeps.borrow(), [Duration::from_secs(7)]);
    assert_eq!(progress(&config).fetched, 1);
}

#[test]
fn server_errors_back_off_and_are_recorded() {
    let calls = Mutex::new(0);
    let stub = Stub::start(move |_| {
        let mut calls = calls.lock().unwrap();
        *calls += 1;
        if *calls <= 2 {
            reply(503, &[], "")
        } else {
            reply(200, &[], stub::RELEASE_JSON)
        }
    });
    let config = setup("503", &stub, &[61]);
    let clock = FakeClock::new();
    assert_eq!(run(&config, &clock, None).unwrap().requests, 3);
    assert_eq!(
        *clock.sleeps.borrow(),
        [Duration::from_secs(5), Duration::from_secs(10)]
    );
    let progress = progress(&config);
    assert_eq!(progress.fetched, 1);
    assert_eq!(
        progress.last_error.unwrap().1,
        "release 61: Discogs answered 503"
    );
}

#[test]
fn a_404_marks_the_release_gone() {
    let stub = Stub::start(|request| {
        if request.release_id() == 71 {
            reply(404, &[], "{\"message\": \"Release not found.\"}")
        } else {
            reply(200, &[], stub::RELEASE_JSON)
        }
    });
    let config = setup("404", &stub, &[71, 72]);
    assert_eq!(run(&config, &FakeClock::new(), None).unwrap().requests, 2);
    let progress = progress(&config);
    assert_eq!((progress.gone, progress.fetched), (1, 1));
}

#[test]
fn a_release_that_always_gets_a_bad_reply_is_failed_and_the_queue_moves_on() {
    let stub = Stub::start(|request| match request.release_id() {
        81 => reply(503, &[], ""),
        82 => reply(410, &[], ""),
        83 => reply(200, &[], "{\"not\": \"a release\"}"),
        _ => reply(200, &[], stub::RELEASE_JSON),
    });
    let config = setup("failed", &stub, &[81, 82, 83, 84]);
    let clock = FakeClock::new();
    // Limited so that a loop stuck on release 81 ends the test.
    let summary = run(&config, &clock, Some(30)).unwrap();
    assert_eq!(summary.stopped, Stopped::Done);
    assert_eq!(summary.requests, 16);
    let progress = progress(&config);
    assert_eq!(
        (progress.failed, progress.fetched, progress.queued),
        (3, 1, 0)
    );
    let status = sync::status(&config, &clock).unwrap().to_string();
    assert!(status.contains("failed: 3\n"), "{status}");
}

#[test]
fn network_errors_do_not_count_against_a_release() {
    // A stub that closes the connection without a reply.
    let stub = Stub::start(|_| String::new());
    let config = setup("network", &stub, &[91]);
    let summary = run(&config, &FakeClock::new(), Some(8)).unwrap();
    assert_eq!(summary.stopped, Stopped::Limit);
    let progress = progress(&config);
    assert_eq!((progress.failed, progress.queued), (0, 1));
}
