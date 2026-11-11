//! The import on copies of the sources crate's fixture set: a whole import and the swap, a
//! resume after a truncated file, a bad checksum, a resume that finds another dump, a
//! damaged build, a build from an older set, and the refusals.

#[path = "support/clock.rs"]
mod clock;
#[path = "support/generated.rs"]
mod generated;
#[path = "../../config/tests/support/setup.rs"]
mod support;

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
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

/// The four dumps' names.
const DUMPS: [&str; 4] = ["artists", "labels", "masters", "releases"];

/// The fixture set's files under the name of the set of `date`.
fn set_files(date: &str) -> Vec<String> {
    let mut files: Vec<String> = DUMPS
        .iter()
        .map(|dump| format!("discogs_{date}_{dump}.xml.gz"))
        .collect();
    files.push(format!("discogs_{date}_CHECKSUM.txt"));
    files
}

impl Setup {
    /// Copies the fixture set of `DATE` in the folder as the set of `date`.
    fn copy_set(&self, date: &str) {
        let listed =
            fs::read_to_string(self.file(&format!("discogs_{DATE}_CHECKSUM.txt"))).unwrap();
        for dump in DUMPS {
            fs::copy(
                self.file(&format!("discogs_{DATE}_{dump}.xml.gz")),
                self.file(&format!("discogs_{date}_{dump}.xml.gz")),
            )
            .unwrap();
        }
        fs::write(
            self.file(&format!("discogs_{date}_CHECKSUM.txt")),
            listed.replace(DATE, date),
        )
        .unwrap();
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
        "catalog built from the set of 20000101: 10 artists, 5 labels, 2 masters, 12 releases"
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
    let credits: Vec<(u64, Vec<u8>)> = db
        .prepare("SELECT artist_id, roles FROM credit WHERE release_id = 1001 ORDER BY artist_id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let role = |blob: &[u8]| -> Vec<String> {
        blob.chunks(4)
            .map(|id| {
                db.query_row(
                    "SELECT name FROM role WHERE id = ?1",
                    [u32::from_le_bytes(id.try_into().unwrap())],
                    |row| row.get(0),
                )
                .unwrap()
            })
            .collect()
    };
    assert_eq!(
        credits
            .iter()
            .map(|(artist, roles)| (*artist, role(roles)))
            .collect::<Vec<_>>(),
        [
            (2, vec!["Producer".to_string(), "Written-By".to_string()]),
            (
                3,
                vec![
                    "Lacquer Cut By".to_string(),
                    "Performer".to_string(),
                    "Producer".to_string()
                ]
            ),
            (5, vec!["Strings".to_string()]),
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
        [
            "1001", "1002", "1003", "2001", "2002", "2003", "2004", "2005", "2006", "2008", "2011",
            "2014"
        ]
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
    assert_eq!(count(&setup.catalog(), "release"), 12);
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
    assert_eq!(count(&setup.catalog(), "label"), 5);
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
    assert_eq!(count(&setup.catalog(), "artist"), 10);
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
    setup.copy_set("20000201");
    let (result, lines) = setup.run(1);
    assert_eq!(
        result.unwrap().to_string(),
        "catalog built from the set of 20000201: 10 artists, 5 labels, 2 masters, 12 releases"
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
fn the_sets_older_than_the_one_imported_are_deleted_after_the_swap() {
    let setup = setup("prune");
    setup.copy_set("20000201");
    // A newer set downloading, and a file that isn't a set's.
    fs::write(setup.file(".discogs_20000301_artists.part"), b"").unwrap();
    fs::write(setup.file("notes.txt"), b"the owner's").unwrap();
    let (result, lines) = setup.run(100);
    assert_eq!(
        result.unwrap().to_string(),
        "catalog built from the set of 20000201: 10 artists, 5 labels, 2 masters, 12 releases"
    );
    let mut kept = set_files("20000201");
    kept.extend([
        ".discogs_20000301_artists.part".to_string(),
        "notes.txt".to_string(),
    ]);
    kept.sort();
    assert_eq!(support::files(&setup.dumps), kept);
    assert!(
        logged(
            &lines,
            "deleted discogs_20000101_releases.xml.gz, older than the set of 20000201"
        ),
        "{lines:?}"
    );
}

#[test]
fn an_import_that_finds_the_catalog_current_deletes_the_older_sets() {
    let setup = setup("prune-current");
    setup.run(100).0.unwrap();
    setup.copy_set("19991201");
    let summary = setup.run(100).0.unwrap();
    assert_eq!(
        summary.to_string(),
        "the catalog is already built from the set of 20000101"
    );
    let mut kept = set_files(DATE);
    kept.sort();
    assert_eq!(support::files(&setup.dumps), kept);
}

#[test]
fn a_set_that_cant_be_deleted_is_logged_and_the_import_still_succeeds() {
    let setup = setup("prune-refused");
    setup.run(100).0.unwrap();
    setup.copy_set("20000201");
    // A folder the service can't delete from, as an unmounted share's would be.
    fs::set_permissions(&setup.dumps, fs::Permissions::from_mode(0o555)).unwrap();
    let (built, built_lines) = setup.run(100);
    let (current, current_lines) = setup.run(100);
    fs::set_permissions(&setup.dumps, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        built.unwrap().to_string(),
        "catalog built from the set of 20000201: 10 artists, 5 labels, 2 masters, 12 releases"
    );
    assert_eq!(
        current.unwrap().to_string(),
        "the catalog is already built from the set of 20000201"
    );
    for lines in [built_lines, current_lines] {
        assert!(
            lines.iter().any(|line| line.starts_with("error: ")
                && line.contains("discogs_20000101_")
                && line.contains("ermission denied")),
            "{lines:?}"
        );
    }
    let mut kept = set_files(DATE);
    kept.extend(set_files("20000201"));
    kept.sort();
    assert_eq!(support::files(&setup.dumps), kept);
}

#[test]
fn a_failed_import_deletes_no_set() {
    let setup = setup("prune-failed");
    setup.copy_set("20000201");
    fs::copy(
        fixtures().join("dumps-truncated/discogs_20000101_releases.xml.gz"),
        setup.file("discogs_20000201_releases.xml.gz"),
    )
    .unwrap();
    setup.run(100).0.unwrap_err();
    let mut kept = set_files(DATE);
    kept.extend(set_files("20000201"));
    kept.sort();
    assert_eq!(support::files(&setup.dumps), kept);
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

/// The import's budgets (plan, phase 3) on 200k generated releases and artists, labels and
/// masters in proportion: at least 40k releases a second, at most 400 bytes a release with
/// indexes. Run with
/// `cargo test --release -p features --test import -- --ignored budget --nocapture` under
/// `/usr/bin/time -l` for the peak memory.
#[test]
#[ignore = "a benchmark: generates and imports 200k releases"]
fn budget_on_200k_generated_releases() {
    use std::time::Instant;
    const RELEASES: u64 = 200_000;
    let setup = setup("budget");
    for entry in fs::read_dir(&setup.dumps).unwrap() {
        fs::remove_file(entry.unwrap().path()).unwrap();
    }
    let bytes = generated::write_set(&setup.dumps, RELEASES);
    println!("releases XML: {} bytes a release", bytes / RELEASES);

    let started = Instant::now();
    let mut marks = Vec::new();
    import::run(
        &setup.config,
        &FakeClock::new(),
        import::BATCH,
        &mut |line| {
            marks.push((started.elapsed(), line.to_string()));
        },
    )
    .unwrap();
    let total = started.elapsed();
    let at = |text: &str| {
        marks
            .iter()
            .find(|(_, line)| line.contains(text))
            .unwrap()
            .0
    };
    let releases_took = at("releases: 200000 records, all of").saturating_sub(at("masters: "));
    let finish_took = total.saturating_sub(at("finishing"));
    let bytes = fs::metadata(setup.config.data_dir.join("catalog.sqlite"))
        .unwrap()
        .len();
    println!(
        "releases: {:.0} a second ({releases_took:?}); finish {finish_took:?}; whole run \
         {total:?}; {} bytes a release with indexes",
        f64::from(u32::try_from(RELEASES).unwrap()) / releases_took.as_secs_f64(),
        bytes / RELEASES
    );
}

#[test]
fn a_build_of_an_older_set_is_deleted_without_being_checked() {
    let setup = setup("stale-unchecked");
    let releases = setup.file("discogs_20000101_releases.xml.gz");
    let whole = fs::read(&releases).unwrap();
    fs::copy(
        fixtures().join("dumps-truncated/discogs_20000101_releases.xml.gz"),
        &releases,
    )
    .unwrap();
    setup.run(1).0.unwrap_err();
    fs::write(&releases, whole).unwrap();
    // Damage a page the build's set isn't read from: a check would find it.
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
    result.unwrap();
    assert!(
        logged(&lines, "begun on the set of 20000101; starting again"),
        "{lines:?}"
    );
    assert!(!logged(&lines, "damaged"), "{lines:?}");
}

#[test]
fn a_catalog_that_cant_be_read_is_replaced() {
    let setup = setup("unreadable-catalog");
    let path = setup.config.data_dir.join("catalog.sqlite");
    fs::write(&path, vec![0xA5; 8192]).unwrap();
    let (result, lines) = setup.run(100);
    result.unwrap();
    assert!(
        logged(&lines, "catalog.sqlite") && logged(&lines, "can't be read"),
        "{lines:?}"
    );
    assert_eq!(count(&setup.catalog(), "release"), 12);
}
