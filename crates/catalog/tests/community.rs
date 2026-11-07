//! The community database's queue order, refresh and reopening.

use std::path::PathBuf;

use catalog::community::{Community, Failure, Progress, Source, Values};

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
fn drain(db: &mut Community, now: i64) -> Vec<u64> {
    let mut order = Vec::new();
    while let Some(id) = db.next_due(now).unwrap() {
        db.record(id, VALUES, now).unwrap();
        order.push(id);
    }
    order
}

const HOUR: i64 = 60 * 60;
const DAY: i64 = 24 * HOUR;

/// Values this old are fetched again.
const REFRESH_AFTER: i64 = 90 * DAY;

#[test]
fn takes_the_library_first_then_candidates_each_in_the_order_queued() {
    let mut db = Community::open(&data_dir("order")).unwrap();
    db.queue(&[30, 10], Source::Candidate).unwrap();
    db.queue(&[5, 20], Source::Library).unwrap();
    db.queue(&[1], Source::Candidate).unwrap();
    assert_eq!(drain(&mut db, 1000), [5, 20, 30, 10, 1]);
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
    assert_eq!(drain(&mut db, 1000), [3, 4, 1, 2]);
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
    assert_eq!(drain(&mut db, now), [1, 2]);
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
            failed: 0,
            gone: 0,
            due: 0,
            paused: true,
            last_error: Some((60, "Discogs answered 503".to_string())),
            failures: vec![],
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

#[test]
fn five_bad_replies_over_an_hour_fail_a_release_until_the_refresh() {
    let mut db = Community::open(&data_dir("failures")).unwrap();
    db.queue(&[1, 2], Source::Library).unwrap();
    // A short outage: any number of bad replies within the hour is retried.
    for count in 1..=8 {
        let failure = db
            .record_failure(1, "Discogs answered 503", 10 + count)
            .unwrap();
        assert_eq!(failure, Failure::Retry(count.cast_unsigned()));
        assert_eq!(db.next_due(10).unwrap(), Some(1));
    }
    // The first was at 11; an hour later, with at least five, the release fails.
    assert_eq!(
        db.record_failure(1, "Discogs answered 503", 11 + HOUR - 1)
            .unwrap(),
        Failure::Retry(9)
    );
    assert_eq!(
        db.record_failure(1, "Discogs answered 503", 11 + HOUR)
            .unwrap(),
        Failure::Failed
    );
    assert_eq!(db.next_due(10).unwrap(), Some(2));
    let progress = db.progress(10).unwrap();
    assert_eq!(
        (progress.failed, progress.fetched, progress.queued),
        (1, 0, 1)
    );

    // Release 2 is fetched; release 1 comes due only at the refresh, and a fetch then
    // replaces its failed row.
    db.record(2, VALUES, 11 + HOUR).unwrap();
    assert_eq!(db.next_due(REFRESH_AFTER + HOUR + 10).unwrap(), None);
    assert_eq!(db.next_due(REFRESH_AFTER + HOUR + 11).unwrap(), Some(1));
    assert_eq!(db.progress(REFRESH_AFTER + HOUR + 11).unwrap().due, 2);
    db.record(1, VALUES, REFRESH_AFTER + HOUR + 11).unwrap();
    let progress = db.progress(REFRESH_AFTER + HOUR + 11).unwrap();
    assert_eq!((progress.failed, progress.fetched), (0, 2));
}

#[test]
fn bad_replies_over_an_hour_but_fewer_than_five_do_not_fail_a_release() {
    let mut db = Community::open(&data_dir("failures-few")).unwrap();
    db.queue(&[1], Source::Library).unwrap();
    for (count, at) in [0, HOUR, 2 * HOUR, 3 * HOUR].into_iter().enumerate() {
        let failure = db.record_failure(1, "bad", at).unwrap();
        assert_eq!(failure, Failure::Retry(count as u64 + 1));
    }
    assert_eq!(
        db.record_failure(1, "bad", 4 * HOUR).unwrap(),
        Failure::Failed
    );
}

#[test]
fn progress_lists_the_ten_newest_failed_releases_with_their_errors() {
    let mut db = Community::open(&data_dir("failed-list")).unwrap();
    let ids: Vec<u64> = (1..=12).collect();
    db.queue(&ids, Source::Library).unwrap();
    for &id in &ids {
        // Failed at 2h, 3h, ... one hour apart; release 12 last.
        db.record_failure(id, "first", 0).unwrap();
        for _ in 0..3 {
            db.record_failure(id, "again", 0).unwrap();
        }
        let at = (id.cast_signed() + 1) * HOUR;
        db.record_failure(id, &format!("error {id}"), at).unwrap();
    }
    let progress = db.progress(13 * HOUR).unwrap();
    assert_eq!(progress.failed, 12);
    let listed: Vec<u64> = progress.failures.iter().map(|(id, _)| *id).collect();
    assert_eq!(listed, [12, 11, 10, 9, 8, 7, 6, 5, 4, 3]);
    assert_eq!(progress.failures[0].1, "error 12");
}

#[test]
fn a_fetch_clears_the_failure_count() {
    let mut db = Community::open(&data_dir("failures-cleared")).unwrap();
    db.queue(&[1], Source::Library).unwrap();
    for _ in 0..4 {
        db.record_failure(1, "bad", 10).unwrap();
    }
    db.record(1, VALUES, 11).unwrap();
    // A refresh that fails starts counting from one again.
    assert_eq!(db.record_failure(1, "bad", 12).unwrap(), Failure::Retry(1));
}

#[test]
fn a_file_with_an_older_shape_is_discarded_and_recreated() {
    let dir = data_dir("old-shape");
    // The tables as an earlier build made them: no `done`, `failed` or `error` columns.
    let old = rusqlite::Connection::open(dir.join("community.sqlite")).unwrap();
    old.execute_batch(
        "CREATE TABLE queue (seq INTEGER PRIMARY KEY, release_id INTEGER NOT NULL UNIQUE,
                             source INTEGER NOT NULL);
         CREATE TABLE result (release_id INTEGER PRIMARY KEY, fetched_at INTEGER NOT NULL,
                              gone INTEGER NOT NULL, rating_average REAL, rating_count INTEGER,
                              have INTEGER, want INTEGER);
         INSERT INTO queue (release_id, source) VALUES (1, 0), (2, 0);
         INSERT INTO result (release_id, fetched_at, gone) VALUES (1, 5, 0);",
    )
    .unwrap();
    drop(old);

    let mut db = Community::open(&dir).unwrap();
    assert_eq!(db.next_due(100).unwrap(), None);
    assert_eq!(db.progress(100).unwrap().queued, 0);
    // The new tables work, and a file of the current shape is kept as it is.
    db.queue(&[3], Source::Library).unwrap();
    db.record_failure(3, "bad", 10).unwrap();
    drop(db);
    let db = Community::open(&dir).unwrap();
    assert_eq!(db.next_due(100).unwrap(), Some(3));
}
