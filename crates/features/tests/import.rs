//! The import on copies of the sources crate's fixture set: a whole import and the swap, a
//! resume after a truncated file, a bad checksum, a resume that finds another dump, a
//! damaged build, a build from an older set, and the refusals.

#[path = "support/clock.rs"]
mod clock;
#[path = "../../config/tests/support/setup.rs"]
mod support;

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use catalog::discogs::{Catalog, Reading};
use clock::{FakeClock, START};
use config::Config;
use features::import::{self, Summary};
use rusqlite::Connection;

const DATE: &str = "20000101";

/// The sources crate's fixtures: one copy, read by its tests and these.
fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../sources/tests/fixtures")
}

struct Setup {
    config: Config,
    dumps: PathBuf,
}

/// A config in a fresh directory named for the test, with `[dumps]` pointing at a copy of
/// the fixture set.
fn setup(test: &str) -> Setup {
    let dir = support::fresh_dir("import", test);
    let (path, _) = support::write_config(&dir);
    let dumps = dir.join("discogs");
    fs::create_dir_all(&dumps).unwrap();
    for entry in fs::read_dir(fixtures().join("dumps")).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), dumps.join(entry.file_name())).unwrap();
    }
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(format!("[dumps]\ndir = \"{}\"\n", dumps.display()).as_bytes())
        .unwrap();
    Setup {
        config: config::load(&path).unwrap(),
        dumps,
    }
}

impl Setup {
    /// Runs the import committing every `batch` records, and returns its result and the
    /// lines it logged.
    fn run(&self, batch: u64) -> (Result<Summary, import::ImportError>, Vec<String>) {
        let mut lines = Vec::new();
        let result = import::run(&self.config, &FakeClock::new(), batch, &mut |line| {
            lines.push(line.to_string());
        });
        (result, lines)
    }

    fn catalog(&self) -> Connection {
        Connection::open(self.config.data_dir.join("catalog.sqlite")).unwrap()
    }

    fn build(&self) -> PathBuf {
        self.config.data_dir.join("catalog.next.sqlite")
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dumps.join(name)
    }
}

fn rows(connection: &Connection, sql: &str) -> Vec<String> {
    connection
        .prepare(sql)
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn count(connection: &Connection, table: &str) -> u64 {
    connection
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

fn logged(lines: &[String], text: &str) -> bool {
    lines.iter().any(|line| line.contains(text))
}

#[test]
fn imports_the_newest_set_and_swaps_the_catalog_in() {
    let setup = setup("whole");
    let (result, lines) = setup.run(2);
    let summary = result.unwrap();
    assert_eq!(
        summary.to_string(),
        "catalog built from the set of 20000101: 5 artists, 3 labels, 2 masters, 3 releases"
    );
    assert!(logged(&lines, "releases: 2 records"), "{lines:?}");
    assert!(!setup.build().exists());
    let Reading::Open(catalog) = Catalog::open_read_only(&setup.config.data_dir).unwrap() else {
        panic!("no catalog");
    };
    let summary = catalog.summary().unwrap();
    assert_eq!(summary.swapped_at, Some(START));
    assert!(summary.imported.iter().all(|i| i.date == DATE));
    let db = setup.catalog();
    // Both sides of the group, once each.
    assert_eq!(
        rows(
            &db,
            "SELECT group_id || '-' || member_id FROM membership ORDER BY 1"
        ),
        ["1-2", "1-3"]
    );
    assert_eq!(
        rows(
            &db,
            "SELECT c.artist_id || ' ' || r.name FROM credit c JOIN role r ON r.id = c.role_id
             WHERE c.release_id = 1001 ORDER BY 1"
        ),
        [
            "2 Producer",
            "2 Written-By",
            "3 Lacquer Cut By",
            "3 Performer",
            "3 Producer",
            "5 Strings"
        ]
    );
    // Three tracks, 4:45, none and 1:02:03, as little-endian u16s.
    let (tracks, durations): (u64, Vec<u8>) = db
        .query_row(
            "SELECT tracks, durations FROM release WHERE id = 1001",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(tracks, 3);
    assert_eq!(durations, [29, 1, 0, 0, 139, 14]);
    // The release with every optional element missing.
    let (master, durations, country): (u64, Option<Vec<u8>>, Option<i64>) = db
        .query_row(
            "SELECT master_id, durations, country_id FROM release WHERE id = 1003",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!((master, durations, country), (0, None, None));
    assert_eq!(count(&db, "barcode"), 2);
}

#[test]
fn a_second_run_on_the_same_set_does_nothing() {
    let setup = setup("current");
    setup.run(100).0.unwrap();
    let before = fs::metadata(setup.config.data_dir.join("catalog.sqlite"))
        .unwrap()
        .modified()
        .unwrap();
    let summary = setup.run(100).0.unwrap();
    assert_eq!(
        summary.to_string(),
        "the catalog is already built from the set of 20000101"
    );
    assert!(!setup.build().exists());
    let after = fs::metadata(setup.config.data_dir.join("catalog.sqlite"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(before, after);
}

#[test]
fn a_truncated_dump_stops_the_import_and_the_next_run_resumes_it() {
    let setup = setup("truncated");
    let releases = setup.file("discogs_20000101_releases.xml.gz");
    let whole = fs::read(&releases).unwrap();
    fs::copy(
        fixtures().join("dumps-truncated/discogs_20000101_releases.xml.gz"),
        &releases,
    )
    .unwrap();
    let (result, _) = setup.run(1);
    let error = result.unwrap_err().to_string();
    assert!(error.contains("truncated"), "{error}");
    assert!(!setup.config.data_dir.join("catalog.sqlite").exists());
    fs::write(&releases, whole).unwrap();
    let (result, lines) = setup.run(1);
    result.unwrap();
    assert!(logged(&lines, "artists: loaded already"), "{lines:?}");
    assert!(
        logged(&lines, "releases: resuming after record 2"),
        "{lines:?}"
    );
    let db = setup.catalog();
    assert_eq!(
        rows(&db, "SELECT CAST(id AS TEXT) FROM release ORDER BY id"),
        ["1001", "1002", "1003"]
    );
}

#[test]
fn a_resume_that_finds_another_record_loads_the_dump_again() {
    let setup = setup("mismatch");
    let releases = setup.file("discogs_20000101_releases.xml.gz");
    let whole = fs::read(&releases).unwrap();
    fs::copy(
        fixtures().join("dumps-truncated/discogs_20000101_releases.xml.gz"),
        &releases,
    )
    .unwrap();
    setup.run(1).0.unwrap_err();
    Connection::open(setup.build())
        .unwrap()
        .execute(
            "UPDATE progress SET last_id = 999 WHERE dump = 'releases'",
            [],
        )
        .unwrap();
    fs::write(&releases, whole).unwrap();
    let (result, lines) = setup.run(1);
    result.unwrap();
    assert!(
        logged(
            &lines,
            "releases: record 2 is 1002, but the build's is 999; loading releases again"
        ),
        "{lines:?}"
    );
    assert_eq!(count(&setup.catalog(), "release"), 3);
}

#[test]
fn a_dump_that_doesnt_match_its_checksum_is_loaded_again_next_time() {
    let setup = setup("checksum");
    let checksum = setup.file("discogs_20000101_CHECKSUM.txt");
    let listed = fs::read_to_string(&checksum).unwrap();
    let labels = listed
        .lines()
        .find(|line| line.contains("labels"))
        .unwrap()
        .to_string();
    let wrong = format!("{}{}", "0".repeat(64), &labels[64..]);
    fs::write(&checksum, listed.replace(&labels, &wrong)).unwrap();
    let error = setup.run(1).0.unwrap_err().to_string();
    assert!(error.contains(&"0".repeat(64)), "{error}");
    let build = Connection::open(setup.build()).unwrap();
    assert_eq!(count(&build, "label"), 0);
    assert_eq!(count(&build, "progress"), 0);
    assert_eq!(rows(&build, "SELECT dump FROM import"), ["artists"]);
    drop(build);
    fs::write(&checksum, listed).unwrap();
    let (result, lines) = setup.run(1);
    result.unwrap();
    assert!(logged(&lines, "artists: loaded already"), "{lines:?}");
    assert_eq!(count(&setup.catalog(), "label"), 3);
}

#[test]
fn a_damaged_build_is_started_again() {
    let setup = setup("damaged");
    let releases = setup.file("discogs_20000101_releases.xml.gz");
    let whole = fs::read(&releases).unwrap();
    fs::copy(
        fixtures().join("dumps-truncated/discogs_20000101_releases.xml.gz"),
        &releases,
    )
    .unwrap();
    setup.run(1).0.unwrap_err();
    let root: usize = {
        let build = Connection::open(setup.build()).unwrap();
        build
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
            .unwrap();
        build
            .query_row(
                "SELECT rootpage FROM sqlite_master WHERE name = 'artist'",
                [],
                |row| row.get(0),
            )
            .unwrap()
    };
    let mut bytes = fs::read(setup.build()).unwrap();
    bytes[(root - 1) * 4096..root * 4096].fill(0xA5);
    fs::write(setup.build(), bytes).unwrap();
    fs::write(&releases, whole).unwrap();
    let (result, lines) = setup.run(1);
    result.unwrap();
    assert!(logged(&lines, "damaged"), "{lines:?}");
    assert!(!logged(&lines, "loaded already"), "{lines:?}");
    assert_eq!(count(&setup.catalog(), "artist"), 5);
}

#[test]
fn a_build_begun_on_an_older_set_starts_again_on_the_newest() {
    let setup = setup("newer");
    let releases = setup.file("discogs_20000101_releases.xml.gz");
    let whole = fs::read(&releases).unwrap();
    fs::copy(
        fixtures().join("dumps-truncated/discogs_20000101_releases.xml.gz"),
        &releases,
    )
    .unwrap();
    setup.run(1).0.unwrap_err();
    fs::write(&releases, whole).unwrap();
    // The same files as a newer month's set.
    let listed = fs::read_to_string(setup.file("discogs_20000101_CHECKSUM.txt")).unwrap();
    for dump in ["artists", "labels", "masters", "releases"] {
        fs::copy(
            setup.file(&format!("discogs_20000101_{dump}.xml.gz")),
            setup.file(&format!("discogs_20000201_{dump}.xml.gz")),
        )
        .unwrap();
    }
    fs::write(
        setup.file("discogs_20000201_CHECKSUM.txt"),
        listed.replace("20000101", "20000201"),
    )
    .unwrap();
    let (result, lines) = setup.run(1);
    assert_eq!(
        result.unwrap().to_string(),
        "catalog built from the set of 20000201: 5 artists, 3 labels, 2 masters, 3 releases"
    );
    assert!(
        logged(&lines, "begun on the set of 20000101; starting again"),
        "{lines:?}"
    );
    let db = setup.catalog();
    assert_eq!(rows(&db, "SELECT DISTINCT date FROM import"), ["20000201"]);
}

#[test]
fn a_failed_import_leaves_the_catalog_in_use_as_it_was() {
    let setup = setup("keeps");
    setup.run(100).0.unwrap();
    let path = setup.config.data_dir.join("catalog.sqlite");
    let before = fs::read(&path).unwrap();
    let listed = fs::read_to_string(setup.file("discogs_20000101_CHECKSUM.txt")).unwrap();
    for dump in ["artists", "labels", "masters"] {
        fs::copy(
            setup.file(&format!("discogs_20000101_{dump}.xml.gz")),
            setup.file(&format!("discogs_20000201_{dump}.xml.gz")),
        )
        .unwrap();
    }
    fs::copy(
        fixtures().join("dumps-truncated/discogs_20000101_releases.xml.gz"),
        setup.file("discogs_20000201_releases.xml.gz"),
    )
    .unwrap();
    fs::write(
        setup.file("discogs_20000201_CHECKSUM.txt"),
        listed.replace("20000101", "20000201"),
    )
    .unwrap();
    setup.run(100).0.unwrap_err();
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn refuses_without_the_dumps_table_and_names_it() {
    let dir = support::fresh_dir("import", "no-table");
    let (path, _) = support::write_config(&dir);
    let config = config::load(&path).unwrap();
    let error = import::run(&config, &FakeClock::new(), 1, &mut |_| {})
        .unwrap_err()
        .to_string();
    assert!(error.contains("[dumps]"), "{error}");
    assert!(error.contains(&path.display().to_string()), "{error}");
}

#[test]
fn refuses_a_folder_without_a_complete_set() {
    let setup = setup("no-set");
    fs::remove_file(setup.file("discogs_20000101_CHECKSUM.txt")).unwrap();
    let error = setup.run(1).0.unwrap_err().to_string();
    assert!(error.contains("no complete set"), "{error}");
    assert!(
        error.contains(&setup.dumps.display().to_string()),
        "{error}"
    );
}

#[test]
fn refuses_while_another_import_runs() {
    let setup = setup("busy");
    let _lock = catalog::build::lock(&setup.config.data_dir).unwrap();
    let error = setup.run(1).0.unwrap_err().to_string();
    assert!(error.contains("another import is running"), "{error}");
    assert!(error.contains("dumps.lock"), "{error}");
}
