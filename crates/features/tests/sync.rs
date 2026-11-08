//! The sync loop against the stub Discogs API, with a made-up clock: nothing waits on
//! real time.

#[path = "../../sources/tests/support/stub.rs"]
mod stub;

use std::cell::{Cell, RefCell};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::panic::{self, AssertUnwindSafe};
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
    /// Called with the number of sleeps so far, after each one.
    on_sleep: Box<dyn Fn(usize)>,
}

impl FakeClock {
    fn new() -> Self {
        Self {
            now: Cell::new(SystemTime::UNIX_EPOCH + Duration::from_secs(START.unsigned_abs())),
            sleeps: RefCell::new(Vec::new()),
            on_sleep: Box::new(|_| {}),
        }
    }

    /// The time slept so far.
    fn slept(&self) -> Duration {
        self.sleeps.borrow().iter().sum()
    }
}

impl Clock for FakeClock {
    fn now(&self) -> SystemTime {
        self.now.get()
    }

    fn sleep(&self, duration: Duration) {
        self.sleeps.borrow_mut().push(duration);
        self.now.set(self.now.get() + duration);
        (self.on_sleep)(self.sleeps.borrow().len());
    }
}

/// The panic message that ends a test's `serve`, which never returns.
const STOP_SERVE: &str = "stop serve";

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
    assert_eq!(progress(&config).with_values, 3);
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
    assert_eq!((progress.with_values, progress.queued), (1, 1));

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
    assert_eq!(clock.slept(), Duration::from_secs(7));
    assert_eq!(progress(&config).with_values, 1);
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
    assert_eq!((progress.gone, progress.with_values), (1, 1));
}

#[test]
fn a_429_with_no_wait_still_waits_and_is_recorded() {
    let calls = Mutex::new(0);
    let stub = Stub::start(move |_| {
        let mut calls = calls.lock().unwrap();
        *calls += 1;
        if *calls <= 3 {
            reply(429, &[("Retry-After", "0")], "{}")
        } else {
            reply(200, &[], stub::RELEASE_JSON)
        }
    });
    let config = setup("429-zero", &stub, &[101]);
    let clock = FakeClock::new();
    assert_eq!(run(&config, &clock, None).unwrap().requests, 4);
    assert_eq!(clock.slept(), Duration::from_secs(5 + 10 + 20));
    assert_eq!(
        progress(&config).last_error.unwrap().1,
        "release 101: rate-limited (429)"
    );
}

#[test]
fn a_pause_during_a_long_wait_stops_the_run_within_five_seconds() {
    let stub = Stub::start(|_| reply(429, &[("Retry-After", "3600")], "{}"));
    let config = Arc::new(setup("pause-in-wait", &stub, &[111]));
    let mut clock = FakeClock::new();
    clock.on_sleep = {
        let config = Arc::clone(&config);
        // `sync pause` from another process, during the third sleep.
        Box::new(move |sleeps| {
            if sleeps == 3 {
                sync::set_paused(&config, true).unwrap();
            }
        })
    };
    let summary = run(&config, &clock, None).unwrap();
    assert_eq!(summary.stopped, Stopped::Paused);
    assert_eq!(summary.requests, 1);
    assert_eq!(clock.slept(), Duration::from_secs(15));
    assert!(clock.sleeps.borrow().iter().all(|s| s.as_secs() <= 5));
}

#[test]
fn an_idle_serve_notices_a_resume_and_new_work_within_15_seconds() {
    let stub = Stub::releases();
    let config = Arc::new(setup("idle", &stub, &[]));
    sync::set_paused(&config, true).unwrap();
    let mut clock = FakeClock::new();
    clock.on_sleep = {
        let config = Arc::clone(&config);
        Box::new(move |sleeps| match sleeps {
            // Queue work while paused: still nothing to do.
            2 => {
                Community::open(&config.data_dir)
                    .unwrap()
                    .queue(&[121], Source::Library)
                    .unwrap();
            }
            4 => sync::set_paused(&config, false).unwrap(),
            // serve never returns; stop it once it is idle again.
            6 => panic!("{STOP_SERVE}"),
            _ => {}
        })
    };
    let stopped = panic::catch_unwind(AssertUnwindSafe(|| {
        sync::serve(&config, &clock, &mut |_| {});
    }))
    .unwrap_err();
    assert_eq!(stopped.downcast_ref::<String>().unwrap(), STOP_SERVE);
    // Four polls while paused, then the run fetched release 121, then two polls.
    assert_eq!(*clock.sleeps.borrow(), [Duration::from_secs(15); 6]);
    assert_eq!(requested(&stub), [121]);
}

#[test]
fn status_gives_the_time_left_as_a_lower_bound_at_a_second_a_request() {
    let stub = Stub::releases();
    let ids: Vec<u64> = (1..=200).collect();
    let config = setup("status-left", &stub, &ids);
    let status = sync::status(&config, &FakeClock::new())
        .unwrap()
        .to_string();
    assert!(
        status.ends_with("time left: at least 3 min at 60 a minute"),
        "{status}"
    );
}

#[test]
fn serve_waits_while_another_sync_holds_the_lock_and_logs_it_once() {
    let stub = Stub::releases();
    let config = Arc::new(setup("serve-busy", &stub, &[141]));
    // Another process syncing: it holds the lock file.
    let other = fs::File::create(config.data_dir.join("sync.lock")).unwrap();
    other.try_lock().unwrap();
    let other = Mutex::new(Some(other));
    let mut clock = FakeClock::new();
    clock.on_sleep = Box::new(move |sleeps| match sleeps {
        3 => drop(other.lock().unwrap().take()),
        // The run got the lock and fetched; serve is idle again.
        4 => panic!("{STOP_SERVE}"),
        _ => {}
    });
    let mut lines = Vec::new();
    let stopped = panic::catch_unwind(AssertUnwindSafe(|| {
        sync::serve(&config, &clock, &mut |line| {
            lines.push(line.to_string());
            // Without a wait between tries, serve would log forever.
            assert!(lines.len() < 20, "serve is busy-looping");
        });
    }))
    .unwrap_err();
    assert_eq!(stopped.downcast_ref::<String>().unwrap(), STOP_SERVE);
    assert_eq!(*clock.sleeps.borrow(), [Duration::from_secs(15); 4]);
    assert_eq!(requested(&stub), [141]);
    let busy = lines.iter().filter(|l| l.contains("another sync")).count();
    assert_eq!(busy, 1, "{lines:?}");
}

#[test]
fn a_wait_reads_only_the_pause_flag_and_keeps_the_release_it_picked() {
    let calls = Mutex::new(0);
    let stub = Stub::start(move |_| {
        let mut calls = calls.lock().unwrap();
        *calls += 1;
        if *calls == 1 {
            reply(429, &[("Retry-After", "20")], "{}")
        } else {
            reply(200, &[], stub::RELEASE_JSON)
        }
    });
    let config = Arc::new(setup("wait-keeps", &stub, &[]));
    Community::open(&config.data_dir)
        .unwrap()
        .queue(&[131], Source::Candidate)
        .unwrap();
    let mut clock = FakeClock::new();
    clock.on_sleep = {
        let config = Arc::clone(&config);
        // A library release is queued during the wait for the retry of 131.
        Box::new(move |sleeps| {
            if sleeps == 2 {
                Community::open(&config.data_dir)
                    .unwrap()
                    .queue(&[132], Source::Library)
                    .unwrap();
            }
        })
    };
    assert_eq!(run(&config, &clock, None).unwrap().requests, 3);
    // 131 is retried as picked; the new library release goes next.
    assert_eq!(requested(&stub), [131, 131, 132]);
}

#[test]
fn a_bad_reply_defers_the_release_and_the_others_go_first() {
    let stub = Stub::start(|request| match request.release_id() {
        201 => reply(503, &[], ""),
        _ => reply(200, &[], stub::RELEASE_JSON),
    });
    let config = setup("deferred", &stub, &[201, 202, 203]);
    let clock = FakeClock::new();
    // 201 is deferred a minute; 202 and 203 don't wait for it, and the run ends with it
    // still waiting.
    let summary = run(&config, &clock, None).unwrap();
    assert_eq!(summary.stopped, Stopped::Done);
    assert_eq!(requested(&stub), [201, 202, 203]);
    assert_eq!(
        progress(&config).last_error.unwrap().1,
        "release 201: Discogs answered 503"
    );

    // Each later bad reply doubles its wait: 1 min, 2, 4, 8. It is sent again exactly when
    // the wait ends, at 1, 3, 7 and 15 minutes from the first reply.
    let elapsed = |clock: &FakeClock| {
        clock
            .now()
            .duration_since(SystemTime::UNIX_EPOCH + Duration::from_secs(START.unsigned_abs()))
            .unwrap()
    };
    for due in [60, 180, 420, 900] {
        clock.sleep(
            Duration::from_secs(due - 1)
                .checked_sub(elapsed(&clock))
                .unwrap(),
        );
        assert_eq!(run(&config, &clock, None).unwrap().requests, 0, "{due}");
        clock.sleep(Duration::from_secs(1));
        assert_eq!(run(&config, &clock, None).unwrap().requests, 1, "{due}");
        assert_eq!(requested(&stub), [201], "{due}");
    }
}

#[test]
fn a_release_is_failed_after_ten_bad_replies_keeping_its_values() {
    let stub = Stub::start(|_| reply(503, &[], ""));
    let config = setup("ten-attempts", &stub, &[211]);
    let values = catalog::community::Values {
        rating_average: 3.0,
        rating_count: 1,
        have: 1,
        want: 1,
    };
    Community::open(&config.data_dir)
        .unwrap()
        .record(211, values, START)
        .unwrap();
    // 91 days on, it is due for a refresh.
    let clock = FakeClock::new();
    clock.sleep(Duration::from_hours(91 * 24));
    for attempt in 1..=10 {
        assert_eq!(run(&config, &clock, None).unwrap().requests, 1, "{attempt}");
        // Past any wait: the longest of the first nine is 256 minutes.
        clock.sleep(Duration::from_hours(5));
    }
    assert_eq!(requested(&stub).len(), 10);
    let progress = progress(&config);
    assert_eq!((progress.failed, progress.with_values), (1, 1));
    let status = sync::status(&config, &clock).unwrap().to_string();
    assert!(
        status.contains("failed: 1\n  release 211: Discogs answered 503\n"),
        "{status}"
    );
    // It comes due again only at the next refresh.
    assert_eq!(run(&config, &clock, None).unwrap().requests, 0);
}

#[test]
fn ten_bad_replies_in_a_row_stop_the_run_naming_the_last() {
    let stub = Stub::start(|_| reply(400, &[], "{}"));
    let ids: Vec<u64> = (301..=315).collect();
    let config = setup("breaker", &stub, &ids);
    let error = run(&config, &FakeClock::new(), None).unwrap_err();
    assert!(matches!(error, SyncError::BadReplies(_)), "{error}");
    assert_eq!(
        error.to_string(),
        "10 bad replies in a row, the last: bad reply: unexpected status 400; \
         check the network, the API and the client"
    );
    assert_eq!(requested(&stub).len(), 10);
    // Each release is only deferred, none failed.
    let progress = progress(&config);
    assert_eq!((progress.failed, progress.queued), (0, 15));
    assert_eq!(progress.last_error.unwrap().1, error.to_string());
}

#[test]
fn a_429_does_not_count_as_a_bad_reply_and_a_fetch_resets_the_count() {
    let calls = Mutex::new(0);
    let stub = Stub::start(move |request| {
        let mut calls = calls.lock().unwrap();
        *calls += 1;
        match (*calls, request.release_id()) {
            // Nine 400s, a 429 that changes nothing, then the tenth 400.
            (10, _) => reply(429, &[("Retry-After", "1")], "{}"),
            // A fetch: the count starts again.
            (_, 318) => reply(200, &[], stub::RELEASE_JSON),
            _ => reply(400, &[], "{}"),
        }
    });
    let ids: Vec<u64> = (301..=330).collect();
    let config = setup("breaker-reset", &stub, &ids);
    let error = run(&config, &FakeClock::new(), None).unwrap_err();
    assert!(matches!(error, SyncError::BadReplies(_)), "{error}");
    // Nine 400s, the 429, the tenth 400 stops it: 11 requests, and none of the fetch.
    assert_eq!(requested(&stub).len(), 11);
    // Without the 429 counting, a fetch resets: nine, a 200, nine more is no stop.
    let stub = Stub::start(|request| match request.release_id() {
        310 => reply(200, &[], stub::RELEASE_JSON),
        _ => reply(400, &[], "{}"),
    });
    let config = setup("breaker-fetch", &stub, &ids);
    let summary = run(&config, &FakeClock::new(), Some(19)).unwrap();
    assert_eq!(summary.stopped, Stopped::Limit);
}

#[test]
fn a_network_error_is_a_bad_reply_for_its_release() {
    // A stub that closes the connection without a reply.
    let stub = Stub::start(|_| String::new());
    let config = setup("network", &stub, &[91]);
    let clock = FakeClock::new();
    let summary = run(&config, &clock, None).unwrap();
    assert_eq!((summary.requests, summary.stopped), (1, Stopped::Done));
    assert!(
        progress(&config)
            .last_error
            .unwrap()
            .1
            .starts_with("release 91: network:")
    );
}

#[test]
fn a_429_leaves_the_release_untouched() {
    let stub = Stub::start(|_| reply(429, &[("Retry-After", "1")], "{}"));
    let config = setup("429-untouched", &stub, &[231, 232]);
    let summary = run(&config, &FakeClock::new(), Some(5)).unwrap();
    assert_eq!(summary.stopped, Stopped::Limit);
    // The same release is retried after each wait, and nothing is deferred or failed.
    assert_eq!(requested(&stub), [231; 5]);
    let progress = progress(&config);
    assert_eq!((progress.failed, progress.queued), (0, 2));
}
