//! Building the Discogs catalog: the build file, its progress, a resume after the process
//! ended, the finish that swaps it in, and the read-only open the status uses.

#[path = "support/dir.rs"]
mod dir;

use catalog::build::{self, Artist, Build, Credit, Format, Label, Master, Part, Release, Started};
use catalog::discogs::Catalog;
use catalog::discogs::Reading;
use catalog::swap::swap_in;
use dir::data_dir;

/// The rows of `table` in the database at `path`, read through a connection of its own.
fn count(path: &std::path::Path, table: &str) -> u64 {
    rusqlite::Connection::open(path)
        .unwrap()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

const DATE: &str = "20000101";
const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn release(id: u64, style: &str) -> Release {
    Release {
        id,
        title: format!("Release {id}"),
        master_id: 0,
        is_main_release: false,
        released: Some("1999-03-00".to_string()),
        year: Some(1999),
        country: Some("UK".to_string()),
        artists: vec![1],
        labels: vec![],
        credits: vec![Credit {
            artist_id: 2,
            role: "Producer".to_string(),
        }],
        formats: vec![Format {
            name: "Vinyl".to_string(),
            qty: 1,
            descriptions: vec!["LP".to_string()],
        }],
        genres: vec!["Rock".to_string()],
        styles: vec![style.to_string()],
        barcodes: vec![],
        durations: vec![285, 0],
    }
}

/// Imports a whole set of one record a dump, ending with `releases`.
fn import_all(build: &mut Build, releases: &[Release]) {
    build.begin().unwrap();
    build
        .add_artist(&Artist {
            id: 1,
            name: "A".to_string(),
            realname: None,
            namevariations: vec![],
            aliases: vec![],
            members: vec![2],
            groups: vec![],
        })
        .unwrap();
    build
        .add_artist(&Artist {
            id: 2,
            name: "B".to_string(),
            realname: None,
            namevariations: vec![],
            aliases: vec![],
            members: vec![],
            groups: vec![1],
        })
        .unwrap();
    build.finish_dump("artists", DATE, SHA, 2, 100).unwrap();
    build.begin().unwrap();
    build
        .add_label(&Label {
            id: 10,
            name: "L".to_string(),
            parent_id: None,
        })
        .unwrap();
    build.finish_dump("labels", DATE, SHA, 1, 101).unwrap();
    build.begin().unwrap();
    build
        .add_master(&Master {
            id: 100,
            main_release: 1,
            title: "M".to_string(),
            year: None,
            artists: vec![1],
            genres: vec![],
            styles: vec![],
        })
        .unwrap();
    build.finish_dump("masters", DATE, SHA, 1, 102).unwrap();
    build.begin().unwrap();
    for release in releases {
        build.add_release(release).unwrap();
    }
    build
        .finish_dump("releases", DATE, SHA, releases.len() as u64, 103)
        .unwrap();
}

#[test]
fn a_finished_build_replaces_the_catalog_and_reads_back() {
    let dir = data_dir("build", "finish");
    let (mut build, started) = Build::open(&dir).unwrap();
    assert_eq!(started, Started::Continuing);
    import_all(
        &mut build,
        &[release(1, "Shoegaze"), release(2, "Shoegaze")],
    );
    build.finish(200, swap_in).unwrap();
    assert!(!dir.join(build::BUILD_FILE_NAME).exists());
    for suffix in ["-wal", "-shm"] {
        assert!(
            !dir.join(format!("catalog.sqlite{suffix}")).exists(),
            "{suffix}"
        );
    }
    let Reading::Open(catalog) = Catalog::open_read_only(&dir).unwrap() else {
        panic!("no catalog");
    };
    let summary = catalog.summary().unwrap();
    assert_eq!(summary.swapped_at, Some(200));
    assert_eq!(
        summary
            .imported
            .iter()
            .map(|i| (i.dump.as_str(), i.date.as_str(), i.records))
            .collect::<Vec<_>>(),
        [
            ("artists", DATE, 2),
            ("labels", DATE, 1),
            ("masters", DATE, 1),
            ("releases", DATE, 2)
        ]
    );
    drop(catalog);
    assert_eq!(count(&dir.join("catalog.sqlite"), "release"), 2);
    assert_eq!(count(&dir.join("catalog.sqlite"), "membership"), 1);
}

#[test]
fn progress_survives_the_process_ending_and_a_clear_starts_the_part_again() {
    let dir = data_dir("build", "resume");
    {
        let (mut build, _) = Build::open(&dir).unwrap();
        build.begin().unwrap();
        build.add_release(&release(1, "Shoegaze")).unwrap();
        build.commit("releases", DATE, 1, 1).unwrap();
        // Written after the commit and never committed: lost when the build drops.
        build.add_release(&release(2, "Dub")).unwrap();
    }
    let (mut build, started) = Build::open(&dir).unwrap();
    assert_eq!(started, Started::Continuing);
    let state = build.state().unwrap();
    assert_eq!(state.imported, []);
    assert_eq!(state.progress.len(), 1);
    let progress = &state.progress[0];
    assert_eq!(
        (progress.dump.as_str(), progress.date.as_str()),
        ("releases", DATE)
    );
    assert_eq!((progress.records_done, progress.last_id), (1, 1));
    // The interned style survives the reopen; the one rolled back is gone.
    build.begin().unwrap();
    build.add_release(&release(2, "Shoegaze")).unwrap();
    build.add_release(&release(3, "Dub")).unwrap();
    build.commit("releases", DATE, 3, 3).unwrap();
    let names: Vec<(i64, String)> = rusqlite::Connection::open(dir.join(build::BUILD_FILE_NAME))
        .unwrap()
        .prepare("SELECT id, name FROM style ORDER BY id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(names, [(1, "Shoegaze".to_string()), (2, "Dub".to_string())]);
    build.clear(Part::Releases, "releases").unwrap();
    let state = build.state().unwrap();
    assert_eq!(state.progress, []);
    assert_eq!(count(&dir.join(build::BUILD_FILE_NAME), "release"), 0);
    assert_eq!(count(&dir.join(build::BUILD_FILE_NAME), "credit"), 0);
}

#[test]
fn a_build_file_of_another_shape_is_deleted_and_started_again() {
    let dir = data_dir("build", "other-build");
    rusqlite::Connection::open(dir.join(build::BUILD_FILE_NAME))
        .unwrap()
        .execute_batch("CREATE TABLE release (id INTEGER PRIMARY KEY);")
        .unwrap();
    let (build, started) = Build::open(&dir).unwrap();
    assert_eq!(started, Started::OtherShape);
    assert_eq!(build.state().unwrap().progress, []);
}

#[test]
fn a_catalog_of_another_shape_is_left_in_place() {
    let dir = data_dir("build", "other-catalog");
    let path = dir.join("catalog.sqlite");
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch("CREATE TABLE release (id INTEGER PRIMARY KEY);")
        .unwrap();
    let before = std::fs::read(&path).unwrap();
    assert!(matches!(
        Catalog::open_read_only(&dir).unwrap(),
        Reading::OtherShape
    ));
    assert!(matches!(
        Catalog::open_read_only(&data_dir("build", "no-catalog")).unwrap(),
        Reading::Missing
    ));
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn a_second_import_lock_is_busy_and_the_probe_sees_the_first() {
    let dir = data_dir("build", "lock");
    assert!(!build::running(&dir).unwrap());
    let lock = build::lock(&dir).unwrap();
    assert!(build::running(&dir).unwrap());
    assert!(matches!(
        build::lock(&dir),
        Err(catalog::community::LockError::Busy(_))
    ));
    drop(lock);
    assert!(!build::running(&dir).unwrap());
}

#[test]
fn the_progress_of_a_build_reads_without_changing_it() {
    let dir = data_dir("build", "read-progress");
    assert!(matches!(build::read(&dir).unwrap(), Reading::Missing));
    let (mut build, _) = Build::open(&dir).unwrap();
    build.begin().unwrap();
    build.add_release(&release(1, "Dub")).unwrap();
    build.commit("releases", DATE, 1, 1).unwrap();
    let Reading::Open(state) = build::read(&dir).unwrap() else {
        panic!("no build");
    };
    assert_eq!(state.progress[0].records_done, 1);
}
