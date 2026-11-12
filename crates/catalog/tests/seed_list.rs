//! The seed lists kept on disk: one file a seed, of the scores' shape, written whole and
//! renamed in; the least recently asked deleted past the cap, and every list of other
//! inputs, a file of another shape and a `.next` left behind deleted with them.

#[path = "support/dir.rs"]
mod dir;

use std::path::Path;
use std::time::{Duration, SystemTime};

use catalog::discogs::Reading;
use catalog::feedback::Kind;
use catalog::score::{Inputs, Row, digest};
use catalog::seed_list::{self, Seed};
use dir::data_dir;

fn inputs(profile: &str) -> Inputs {
    Inputs {
        catalog_set: "20000101".to_string(),
        catalog_swapped_at: 100,
        profile_digest: digest(profile),
        scoring_version: 1,
    }
}

/// Writes a list of one candidate, `id`, for `seed` from `inputs`, asked at `at` seconds.
fn write(data_dir: &Path, seed: &Seed<'_>, id: u64, inputs: &Inputs, at: u64) {
    let mut writer = seed_list::writer(data_dir, seed).unwrap();
    writer
        .add(&Row {
            kind: Kind::Release,
            id,
            representative: Some(id),
            score: 1.0,
            title: format!("Album {id}"),
            year: None,
            artists: Vec::new(),
            label: None,
            styles: Vec::new(),
            reasons: Vec::new(),
        })
        .unwrap();
    writer.finish(inputs, 1, 200).unwrap();
    asked(data_dir, seed, at);
}

fn asked(data_dir: &Path, seed: &Seed<'_>, at: u64) {
    seed_list::asked(
        data_dir,
        seed,
        SystemTime::UNIX_EPOCH + Duration::from_secs(at),
    )
    .unwrap();
}

/// The one candidate of `seed`'s list, or `None` when there is no list.
fn read(data_dir: &Path, seed: &Seed<'_>) -> Option<u64> {
    match seed_list::open(data_dir, seed).unwrap() {
        Reading::Open(scores) => Some(scores.page(0, 10).unwrap()[0].id),
        Reading::Missing => None,
        Reading::OtherShape => panic!("a seed list of another shape"),
    }
}

#[test]
fn a_seed_s_list_is_read_back_by_its_seed_and_no_other() {
    let dir = data_dir("seed_list", "by-seed");
    assert_eq!(read(&dir, &Seed::Album(2001)), None);
    write(&dir, &Seed::Album(2001), 7, &inputs("a"), 1);
    write(&dir, &Seed::Artist("cora seed"), 8, &inputs("a"), 1);
    assert_eq!(read(&dir, &Seed::Album(2001)), Some(7));
    assert_eq!(read(&dir, &Seed::Artist("cora seed")), Some(8));
    assert_eq!(read(&dir, &Seed::Album(2002)), None);
    assert_eq!(read(&dir, &Seed::Artist("ann example")), None);
    let Reading::Open(scores) = seed_list::open(&dir, &Seed::Album(2001)).unwrap() else {
        panic!("no list");
    };
    assert_eq!(scores.computed().unwrap().inputs, inputs("a"));
    // Written again, it is replaced whole.
    write(&dir, &Seed::Album(2001), 9, &inputs("b"), 2);
    assert_eq!(read(&dir, &Seed::Album(2001)), Some(9));
}

#[test]
fn the_least_recently_asked_past_the_cap_are_deleted() {
    let dir = data_dir("seed_list", "cap");
    let current = inputs("now");
    write(&dir, &Seed::Album(1), 1, &current, 10);
    write(&dir, &Seed::Album(2), 2, &current, 20);
    write(&dir, &Seed::Album(3), 3, &current, 30);
    // The first is asked again, so the second is now the least recently asked.
    asked(&dir, &Seed::Album(1), 40);
    seed_list::sweep(&dir, 2, &current, &mut |_| {}).unwrap();
    assert_eq!(read(&dir, &Seed::Album(1)), Some(1));
    assert_eq!(read(&dir, &Seed::Album(2)), None);
    assert_eq!(read(&dir, &Seed::Album(3)), Some(3));
}

#[test]
fn lists_of_other_inputs_other_shapes_and_unfinished_writes_are_deleted() {
    let dir = data_dir("seed_list", "other");
    let current = inputs("now");
    write(&dir, &Seed::Album(1), 1, &current, 10);
    write(&dir, &Seed::Album(2), 2, &inputs("before"), 20);
    // A file of another shape, and a write that stopped before its rename.
    let lists = dir.join("seed-lists");
    let other = rusqlite::Connection::open(lists.join("album-3.sqlite")).unwrap();
    other
        .execute_batch("CREATE TABLE candidate (rank INTEGER PRIMARY KEY)")
        .unwrap();
    drop(other);
    assert!(matches!(
        seed_list::open(&dir, &Seed::Album(3)).unwrap(),
        Reading::OtherShape
    ));
    drop(seed_list::writer(&dir, &Seed::Album(4)).unwrap());
    assert!(lists.join("album-4.next.sqlite").exists());
    // A list of the current shape whose `computed` row is gone.
    write(&dir, &Seed::Album(5), 5, &current, 50);
    rusqlite::Connection::open(lists.join("album-5.sqlite"))
        .unwrap()
        .execute("DELETE FROM computed", [])
        .unwrap();
    let mut said = Vec::new();
    seed_list::sweep(&dir, 32, &current, &mut |line| said.push(line.to_string())).unwrap();
    assert_eq!(read(&dir, &Seed::Album(1)), Some(1));
    assert_eq!(read(&dir, &Seed::Album(2)), None);
    assert!(!lists.join("album-5.sqlite").exists());
    assert_eq!(said.len(), 2, "{said:?}");
    assert!(
        said.iter()
            .any(|line| line.contains("album-5.sqlite") && line.contains("can't be read")),
        "{said:?}"
    );
    assert!(
        said.iter()
            .any(|line| line.contains("album-3.sqlite") && line.contains("another shape")),
        "{said:?}"
    );
    assert!(!lists.join("album-3.sqlite").exists());
    assert!(!lists.join("album-4.next.sqlite").exists());
}

/// How long a test waits for what it expects, so a missing one fails it.
const WAIT: Duration = Duration::from_secs(10);

#[test]
fn the_lock_waits_for_the_computation_that_holds_it() {
    let dir = data_dir("seed_list", "lock");
    let held = seed_list::lock(&dir, || panic!("nothing holds it yet")).unwrap();
    let (waiting, waited) = std::sync::mpsc::channel();
    let (taken, took) = std::sync::mpsc::channel();
    let thread_dir = dir.clone();
    let second = std::thread::spawn(move || {
        let lock = seed_list::lock(&thread_dir, || waiting.send(()).unwrap()).unwrap();
        taken.send(()).unwrap();
        drop(lock);
    });
    // It says it waits, and it hasn't taken the lock while the first holds it.
    waited.recv_timeout(WAIT).unwrap();
    assert!(took.try_recv().is_err());
    drop(held);
    took.recv_timeout(WAIT).unwrap();
    second.join().unwrap();
}

#[test]
fn asking_for_a_list_that_is_gone_records_nothing() {
    let dir = data_dir("seed_list", "asked-gone");
    let seed = Seed::Album(2001);
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(5);
    assert!(!seed_list::asked(&dir, &seed, at).unwrap());
    write(&dir, &seed, 7, &inputs("a"), 1);
    assert!(seed_list::asked(&dir, &seed, at).unwrap());
}

#[test]
fn the_sweep_keeps_a_list_that_fails_to_open_for_a_reason_other_than_damage() {
    use std::os::unix::fs::PermissionsExt;
    let dir = data_dir("seed_list", "sweep-unopenable");
    let current = inputs("now");
    write(&dir, &Seed::Album(1), 1, &current, 10);
    let lists = dir.join("seed-lists");
    let garbage = lists.join("album-2.sqlite");
    std::fs::write(
        &garbage,
        b"this is not a SQLite database, and is longer than a header",
    )
    .unwrap();
    let file = lists.join("album-1.sqlite");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).unwrap();
    let mut said = Vec::new();
    let swept = seed_list::sweep(&dir, 32, &current, &mut |line| said.push(line.to_string()));
    // The file may be gone, which the asserts below say.
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).ok();
    swept.unwrap();
    // The file that is no database is deleted; the one that couldn't be opened is left, said.
    assert!(!garbage.exists());
    assert!(file.exists());
    assert!(
        said.iter()
            .any(|line| line.contains("album-1.sqlite") && line.contains("skipping")),
        "{said:?}"
    );
    assert_eq!(read(&dir, &Seed::Album(1)), Some(1));
}
