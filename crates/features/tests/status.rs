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
use features::clock::Clock;
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

    let lock = Community::lock(&config.data_dir).unwrap();
    let running = status::read(&config, &clock).unwrap();
    assert!(running.sync.running);
    assert_eq!(running.sync.progress.yielding_since, Some(START - 300));
    // The check let go of the lock file: the sync's lock is still the only one, and once
    // it drops another sync can take it.
    drop(lock);
    Community::lock(&config.data_dir).unwrap();
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
    let _lock = Community::lock(&config.data_dir).unwrap();
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
time left: at least 4 s at 60 a minute
dumps: off, the config has no [dumps] table
import running: no
catalog: none yet"
    );
}

/// The community database as the build before this one wrote it, with another shape's tables.
const PREVIOUS_SCHEMA: &[u8] = include_bytes!("fixtures/previous-schema.sqlite");

#[test]
fn a_missing_database_reads_as_no_sync_and_is_not_created() {
    let config = setup("missing");
    let before = support::files(&config.data_dir);
    let status = status::read(&config, &FakeClock::new()).unwrap();
    assert_eq!(support::files(&config.data_dir), before);
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
    let before = support::files(&config.data_dir);
    let status = status::read(&config, &FakeClock::new()).unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), PREVIOUS_SCHEMA);
    assert_eq!(support::files(&config.data_dir), before);
    assert!(status.sync.other_shape);
    assert_eq!(status.sync.progress.total(), Counts::default());
    let printed = status.to_string();
    assert!(
        printed.contains("database: its tables are from another version"),
        "{printed}"
    );
}

/// The sources crate's fixture set.
fn fixture_set() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../sources/tests/fixtures")
}

/// A config with `[dumps]` pointing at a copy of the fixture set, in a fresh directory.
fn setup_dumps(test: &str) -> (Config, std::path::PathBuf) {
    let dir = support::fresh_dir("status", test);
    let (path, _) = support::write_config(&dir);
    let dumps = dir.join("discogs");
    std::fs::create_dir_all(&dumps).unwrap();
    for entry in std::fs::read_dir(fixture_set().join("dumps")).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), dumps.join(entry.file_name())).unwrap();
    }
    let config_text = std::fs::read_to_string(&path).unwrap()
        + "[dumps]\ndir = \""
        + &dumps.display().to_string()
        + "\"\n";
    std::fs::write(&path, config_text).unwrap();
    (config::load(&path).unwrap(), dumps)
}

/// The dumps' lines of what `sync status` prints.
fn dumps_lines(status: &status::Status) -> String {
    let printed = status.to_string();
    let start = printed.find("dumps:").unwrap();
    printed[start..].to_string()
}

#[test]
fn the_catalog_in_use_and_its_counts() {
    let (config, dumps) = setup_dumps("catalog");
    features::import::run(&config, &FakeClock::new(), 100, &mut |_| {}).unwrap();
    let clock = FakeClock::new();
    clock.sleep(Duration::from_hours(3));
    let status = status::read(&config, &clock).unwrap();
    assert_eq!(status.dumps.newest_set, Ok(Some("20000101".to_string())));
    assert_eq!(status.dumps.running, Ok(false));
    assert_eq!(
        dumps_lines(&status),
        format!(
            "dumps: {}, newest complete set 20000101
import running: no
catalog: the set of 20000101, swapped in 3 h 0 min ago
  artists: 5 in the catalog
  labels: 3 in the catalog
  masters: 2 in the catalog
  releases: 3 in the catalog",
            dumps.display()
        )
    );
}

#[test]
fn a_build_in_progress_against_the_catalogs_counts() {
    let (config, dumps) = setup_dumps("build");
    features::import::run(&config, &FakeClock::new(), 100, &mut |_| {}).unwrap();
    // A newer set whose releases dump ends after its second record.
    let listed = std::fs::read_to_string(dumps.join("discogs_20000101_CHECKSUM.txt")).unwrap();
    for dump in ["artists", "labels", "masters"] {
        std::fs::copy(
            dumps.join(format!("discogs_20000101_{dump}.xml.gz")),
            dumps.join(format!("discogs_20000201_{dump}.xml.gz")),
        )
        .unwrap();
    }
    std::fs::copy(
        fixture_set().join("dumps-truncated/discogs_20000101_releases.xml.gz"),
        dumps.join("discogs_20000201_releases.xml.gz"),
    )
    .unwrap();
    std::fs::write(
        dumps.join("discogs_20000201_CHECKSUM.txt"),
        listed.replace("20000101", "20000201"),
    )
    .unwrap();
    features::import::run(&config, &FakeClock::new(), 1, &mut |_| {}).unwrap_err();
    let lock = catalog::build::lock(&config.data_dir).unwrap();
    let status = status::read(&config, &FakeClock::new()).unwrap();
    assert_eq!(status.dumps.running, Ok(true));
    let lines = dumps_lines(&status);
    assert!(
        lines.ends_with(
            "build: the set of 20000201
  artists: 5 of 5, loaded
  labels: 3 of 3, loaded
  masters: 2 of 2, loaded
  releases: 2 of 3"
        ),
        "{lines}"
    );
    assert!(lines.contains("import running: yes"), "{lines}");
    drop(lock);
}

#[test]
fn a_dumps_folder_that_cant_be_read_is_shown_not_failed() {
    let (config, dumps) = setup_dumps("unmounted");
    std::fs::remove_dir_all(&dumps).unwrap();
    let status = status::read(&config, &FakeClock::new()).unwrap();
    let error = status.dumps.newest_set.clone().unwrap_err();
    assert!(error.contains(&dumps.display().to_string()), "{error}");
    assert!(
        dumps_lines(&status).starts_with(&format!("dumps: {}: ", dumps.display())),
        "{status}"
    );
}

#[test]
fn a_catalog_of_another_shape_is_left_alone_and_the_status_says_so() {
    let config = setup("other-catalog");
    let file = config.data_dir.join("catalog.sqlite");
    std::fs::write(&file, PREVIOUS_SCHEMA).unwrap();
    let before = support::files(&config.data_dir);
    let status = status::read(&config, &FakeClock::new()).unwrap();
    assert_eq!(support::files(&config.data_dir), before);
    assert!(
        status
            .to_string()
            .contains("catalog: its tables are from another version"),
        "{status}"
    );
}

#[test]
fn a_catalog_that_cant_be_read_is_shown_not_failed() {
    let config = setup("unreadable-catalog");
    std::fs::write(config.data_dir.join("catalog.sqlite"), vec![0xA5; 8192]).unwrap();
    std::fs::write(
        config.data_dir.join("catalog.next.sqlite"),
        vec![0xA5; 8192],
    )
    .unwrap();
    let status = status::read(&config, &FakeClock::new()).unwrap();
    let printed = status.to_string();
    assert!(printed.contains("library: "), "{printed}");
    assert!(
        printed.contains("\ncatalog: can't be read: ") && printed.contains("not a database"),
        "{printed}"
    );
    assert!(printed.contains("\nbuild: can't be read: "), "{printed}");
}
