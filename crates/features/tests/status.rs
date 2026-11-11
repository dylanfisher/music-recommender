//! The status on a fixture database written through `Community`, at a made-up time: the
//! counts by source, the yield, the hour's requests and what `sync status` prints.

#[path = "support/clock.rs"]
mod clock;
#[path = "../../config/tests/support/setup.rs"]
mod support;

use std::time::{Duration, SystemTime};

use catalog::community::{Community, Counts, Source, Values};
use clock::{FakeClock, START};
use config::Config;
use features::status;

const VALUES: Values = Values {
    rating_average: 3.5,
    rating_count: 4,
    have: 10,
    want: 2,
    num_for_sale: None,
    lowest_price: None,
};

/// A config in a fresh directory named for the test, with an empty community database.
fn setup(test: &str) -> Config {
    let (path, _) = support::write_config(&support::fresh_dir("status", test));
    config::load(&path).unwrap()
}

/// `seconds` after the Unix epoch.
fn at(seconds: i64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(seconds.unsigned_abs())
}

/// The library: 1 fetched, 2 gone, 3 failed, 4 and 5 never fetched. The candidates: 10
/// fetched 91 days ago and so due, 11 gone, 12 never fetched.
fn fixture(config: &Config) {
    let mut db = Community::open(&config.data_dir).unwrap();
    db.queue(&[1, 2, 3, 4, 5], Source::Library).unwrap();
    db.queue(&[10, 11, 12], Source::Candidate).unwrap();
    db.record(1, VALUES, "{}", START - 600).unwrap();
    db.record_gone(2, START - 600).unwrap();
    for attempt in 0..10 {
        db.record_failure(3, "unexpected status 418", START - 100 + attempt)
            .unwrap();
    }
    db.record(10, VALUES, "{}", START - 91 * 24 * 3600).unwrap();
    db.record_gone(11, START - 600).unwrap();
}

#[test]
fn counts_the_library_and_the_candidates_apart() {
    let config = setup("by-source");
    fixture(&config);
    let status = status::read(&config, &FakeClock::new()).unwrap();
    let progress = &status.sync.progress;
    assert_eq!(
        progress.library,
        Counts {
            queued: 2,
            with_values: 1,
            failed: 1,
            gone: 1,
            due: 0,
        }
    );
    assert_eq!(
        progress.candidates,
        Counts {
            queued: 1,
            with_values: 1,
            failed: 0,
            gone: 1,
            due: 1,
        }
    );
    assert_eq!(
        progress.failures,
        [(3, "unexpected status 418".to_string())]
    );
    assert_eq!(
        progress.last_error,
        Some((START - 91, "release 3: unexpected status 418".to_string()))
    );
}

#[test]
fn shows_the_yield_only_while_a_sync_holds_the_lock() {
    let config = setup("yield");
    // What a sync killed while yielding leaves behind.
    Community::open(&config.data_dir)
        .unwrap()
        .set_yielding(Some(START - 300))
        .unwrap();
    let clock = FakeClock::new();
    let idle = status::read(&config, &clock).unwrap();
    assert!(!idle.sync.running);
    assert_eq!(idle.sync.progress.yielding_since, None);

    let lock = Community::lock(&config.data_dir, std::thread::sleep).unwrap();
    let running = status::read(&config, &clock).unwrap();
    assert!(running.sync.running);
    assert_eq!(running.sync.progress.yielding_since, Some(START - 300));
    // The check let go of the lock file: the sync's lock is still the only one, and once
    // it drops another sync can take it.
    drop(lock);
    Community::lock(&config.data_dir, std::thread::sleep).unwrap();
}

#[test]
fn counts_the_requests_of_the_last_hour() {
    let config = setup("hour");
    let mut db = Community::open(&config.data_dir).unwrap();
    let hour = Duration::from_hours(1);
    for sent in [START - 3600, START - 3599, START - 60, START] {
        db.record_send(at(sent), hour).unwrap();
    }
    let status = status::read(&config, &FakeClock::new()).unwrap();
    assert_eq!(status.sync.requests, 3);
}

#[test]
fn sync_status_prints_every_count() {
    let config = setup("printed");
    fixture(&config);
    let db = Community::open(&config.data_dir).unwrap();
    db.set_yielding(Some(START - 300)).unwrap();
    let _lock = Community::lock(&config.data_dir, std::thread::sleep).unwrap();
    let printed = status::read(&config, &FakeClock::new())
        .unwrap()
        .to_string();
    assert_eq!(
        printed,
        "paused: no
running: yes
yielding to another client: for 5 min
library: 2 queued, 1 with values, 1 failed, 1 gone, 0 due for refresh
candidates: 1 queued, 1 with values, 0 failed, 1 gone, 1 due for refresh
failed releases, newest first:
  release 3: unexpected status 418
requests in the last hour: 0
last error: 1 min ago: release 3: unexpected status 418
time left: at least 4 s at 60 a minute"
    );
}

/// The community database as the build before this one wrote it, with another shape's tables.
const PREVIOUS_SCHEMA: &[u8] = include_bytes!("fixtures/previous-schema.sqlite");

/// The names of the files in `dir`, sorted.
fn files(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn a_missing_database_reads_as_no_sync_and_is_not_created() {
    let config = setup("missing");
    let before = files(&config.data_dir);
    let status = status::read(&config, &FakeClock::new()).unwrap();
    assert_eq!(files(&config.data_dir), before);
    assert_eq!(status.sync.progress.total(), Counts::default());
    assert_eq!(status.sync.requests, 0);
    assert!(!status.sync.other_shape);
    assert!(!status.to_string().contains("database"));
}

#[test]
fn a_database_of_another_shape_is_left_alone_and_the_status_says_so() {
    let config = setup("other-shape");
    let file = config.data_dir.join("community.sqlite");
    std::fs::write(&file, PREVIOUS_SCHEMA).unwrap();
    let before = files(&config.data_dir);
    let status = status::read(&config, &FakeClock::new()).unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), PREVIOUS_SCHEMA);
    assert_eq!(files(&config.data_dir), before);
    assert!(status.sync.other_shape);
    assert_eq!(status.sync.progress.total(), Counts::default());
    let printed = status.to_string();
    assert!(
        printed.contains("database: its tables are from another version"),
        "{printed}"
    );
}
