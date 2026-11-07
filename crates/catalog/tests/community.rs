//! The community database's queue order, refresh and reopening.

use std::path::PathBuf;

use catalog::community::{Community, Progress, Source, Values};

const VALUES: Values = Values {
    rating_average: 3.5,
    rating_count: 4,
    have: 10,
    want: 2,
};

/// A fresh, empty data directory named for the test.
fn data_dir(test: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("community")
        .join(test);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Takes every release due at `now`, recording each as fetched then, and returns the ids
/// in the order they came.
fn drain(db: &Community, now: i64) -> Vec<u64> {
    let mut order = Vec::new();
    while let Some(id) = db.next_due(now).unwrap() {
        db.record(id, VALUES, now).unwrap();
        order.push(id);
    }
    order
}

const DAY: i64 = 24 * 60 * 60;

/// Values this old are fetched again.
const REFRESH_AFTER: i64 = 90 * DAY;

#[test]
fn takes_the_library_first_then_candidates_each_in_the_order_queued() {
    let mut db = Community::open(&data_dir("order")).unwrap();
    db.queue(&[30, 10], Source::Candidate).unwrap();
    db.queue(&[5, 20], Source::Library).unwrap();
    db.queue(&[1], Source::Candidate).unwrap();
    assert_eq!(drain(&db, 1000), [5, 20, 30, 10, 1]);
    assert_eq!(db.next_due(1000).unwrap(), None);
}

#[test]
fn queueing_an_id_twice_keeps_one_entry_and_promotes_a_candidate() {
    let mut db = Community::open(&data_dir("twice")).unwrap();
    assert_eq!(db.queue(&[1, 2, 3], Source::Candidate).unwrap(), 3);
    assert_eq!(db.queue(&[2, 2], Source::Candidate).unwrap(), 0);
    assert_eq!(db.queue(&[4, 3], Source::Library).unwrap(), 1);
    // A library id queued again as a candidate stays in the library.
    assert_eq!(db.queue(&[4], Source::Candidate).unwrap(), 0);
    assert_eq!(drain(&db, 1000), [3, 4, 1, 2]);
}

#[test]
fn a_value_comes_due_after_the_refresh_age_behind_anything_never_fetched() {
    let mut db = Community::open(&data_dir("refresh")).unwrap();
    db.queue(&[1], Source::Library).unwrap();
    db.record(1, VALUES, 0).unwrap();
    db.queue(&[2], Source::Library).unwrap();
    db.record(2, VALUES, DAY).unwrap();

    assert_eq!(db.next_due(REFRESH_AFTER - 1).unwrap(), None);
    assert_eq!(db.progress(REFRESH_AFTER - 1).unwrap().due, 0);

    // Release 1 is due; a release queued now still goes first.
    db.queue(&[3], Source::Candidate).unwrap();
    let now = REFRESH_AFTER + DAY;
    assert_eq!(db.progress(now).unwrap().due, 2);
    assert_eq!(db.next_due(now).unwrap(), Some(3));
    db.record(3, VALUES, now).unwrap();
    // Then the oldest values first.
    assert_eq!(drain(&db, now), [1, 2]);
    assert_eq!(db.progress(now).unwrap().due, 0);
}

#[test]
fn a_release_answered_404_is_gone_and_not_fetched_again() {
    let mut db = Community::open(&data_dir("gone")).unwrap();
    db.queue(&[1, 2], Source::Library).unwrap();
    db.record_gone(1, 0).unwrap();
    db.record(2, VALUES, 0).unwrap();
    let later = REFRESH_AFTER * 2;
    assert_eq!(db.next_due(later).unwrap(), Some(2));
    let progress = db.progress(later).unwrap();
    assert_eq!((progress.fetched, progress.gone, progress.due), (1, 1, 1));
}

#[test]
fn reopening_the_file_keeps_the_queue_results_and_state() {
    let dir = data_dir("reopen");
    {
        let mut db = Community::open(&dir).unwrap();
        db.queue(&[1, 2, 3], Source::Library).unwrap();
        db.record(1, VALUES, 50).unwrap();
        db.set_paused(true).unwrap();
        db.record_error("Discogs answered 503", 60).unwrap();
    }
    let db = Community::open(&dir).unwrap();
    assert_eq!(db.next_due(100).unwrap(), Some(2));
    assert_eq!(
        db.progress(100).unwrap(),
        Progress {
            queued: 2,
            fetched: 1,
            gone: 0,
            due: 0,
            paused: true,
            last_error: Some((60, "Discogs answered 503".to_string())),
        }
    );
    db.set_paused(false).unwrap();
    assert!(!db.paused().unwrap());
}

#[test]
fn refuses_a_data_directory_that_does_not_exist() {
    let dir = data_dir("missing").join("absent");
    let message = Community::open(&dir).err().unwrap().to_string();
    assert!(message.contains("absent"), "{message}");
}
