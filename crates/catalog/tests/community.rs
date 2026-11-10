//! The community database's queue order, refresh and reopening.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use catalog::community::{Community, Failure, Progress, Sent, Source, Values};

const VALUES: Values = Values {
    rating_average: 3.5,
    rating_count: 4,
    have: 10,
    want: 2,
    num_for_sale: Some(3),
    lowest_price: Some(7.25),
};

/// A reply's JSON, as the sync stores it beside its values.
const REPLY: &str = r#"{"id": 1, "community": {"have": 10, "want": 2}}"#;

/// A fresh, empty data directory named for the test.
fn data_dir(test: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("community")
        .join(test);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The request a reply at `now` answers, sent a second before it.
fn sent(now: i64) -> Sent {
    Sent {
        at: SystemTime::UNIX_EPOCH + Duration::from_secs((now - 1).unsigned_abs()),
        keep: Duration::from_secs(65),
    }
}

/// Takes every release due at `now`, recording each as fetched then, and returns the ids
/// in the order they came.
fn drain(db: &mut Community, now: i64) -> Vec<u64> {
    let mut order = Vec::new();
    while let Some(id) = db.next_due(now).unwrap() {
        db.record(id, VALUES, REPLY, now, sent(now)).unwrap();
        order.push(id);
    }
    order
}

const HOUR: i64 = 60 * 60;
const DAY: i64 = 24 * HOUR;

/// Values this old are fetched again.
const REFRESH_AFTER: i64 = catalog::community::REFRESH_AFTER.as_secs().cast_signed();

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
    db.record(1, VALUES, REPLY, 0, sent(0)).unwrap();
    db.queue(&[2], Source::Library).unwrap();
    db.record(2, VALUES, REPLY, DAY, sent(DAY)).unwrap();

    assert_eq!(db.next_due(REFRESH_AFTER - 1).unwrap(), None);
    assert_eq!(db.progress(REFRESH_AFTER - 1).unwrap().due, 0);

    // Release 1 is due; a release queued now still goes first.
    db.queue(&[3], Source::Candidate).unwrap();
    let now = REFRESH_AFTER + DAY;
    assert_eq!(db.progress(now).unwrap().due, 2);
    assert_eq!(db.next_due(now).unwrap(), Some(3));
    db.record(3, VALUES, REPLY, now, sent(now)).unwrap();
    // Then the oldest values first.
    assert_eq!(drain(&mut db, now), [1, 2]);
    assert_eq!(db.progress(now).unwrap().due, 0);
}

#[test]
fn a_release_answered_404_is_gone_and_not_fetched_again() {
    let mut db = Community::open(&data_dir("gone")).unwrap();
    db.queue(&[1, 2], Source::Library).unwrap();
    db.record_gone(1, 0, sent(0)).unwrap();
    db.record(2, VALUES, REPLY, 0, sent(0)).unwrap();
    let later = REFRESH_AFTER * 2;
    assert_eq!(db.next_due(later).unwrap(), Some(2));
    let progress = db.progress(later).unwrap();
    assert_eq!(
        (progress.with_values, progress.gone, progress.due),
        (1, 1, 1)
    );
}

#[test]
fn reopening_the_file_keeps_the_queue_results_and_state() {
    let dir = data_dir("reopen");
    {
        let mut db = Community::open(&dir).unwrap();
        db.queue(&[1, 2, 3], Source::Library).unwrap();
        db.record(1, VALUES, REPLY, 50, sent(50)).unwrap();
        db.set_paused(true).unwrap();
        db.record_error("Discogs answered 503", 60, None).unwrap();
    }
    let db = Community::open(&dir).unwrap();
    assert_eq!(db.next_due(100).unwrap(), Some(2));
    assert_eq!(
        db.progress(100).unwrap(),
        Progress {
            queued: 2,
            with_values: 1,
            failed: 0,
            gone: 0,
            due: 0,
            paused: true,
            last_error: Some((60, "Discogs answered 503".to_string())),
            yielding_since: None,
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

/// Writes a database file with the tables as an earlier build made them, references
/// included, and a fetched release.
fn write_previous_schema(dir: &std::path::Path) {
    let old = rusqlite::Connection::open(dir.join("community.sqlite")).unwrap();
    old.execute_batch(
        "CREATE TABLE queue (seq INTEGER PRIMARY KEY, release_id INTEGER NOT NULL UNIQUE,
                             source INTEGER NOT NULL);
         CREATE INDEX queue_order ON queue (source, seq);
         CREATE TABLE result (
             release_id INTEGER PRIMARY KEY REFERENCES queue (release_id),
             fetched_at INTEGER NOT NULL, gone INTEGER NOT NULL,
             failed INTEGER NOT NULL DEFAULT 0, error TEXT,
             rating_average REAL, rating_count INTEGER, have INTEGER, want INTEGER);
         CREATE INDEX result_fetched_at ON result (fetched_at) WHERE gone = 0;
         CREATE TABLE failures (
             release_id INTEGER PRIMARY KEY REFERENCES queue (release_id),
             count INTEGER NOT NULL);
         CREATE TABLE state (key TEXT PRIMARY KEY, value NOT NULL);
         INSERT INTO queue (release_id, source) VALUES (1, 0), (2, 0);
         INSERT INTO result (release_id, fetched_at, gone, rating_average, rating_count, have,
                             want) VALUES (1, 5, 0, 4.0, 3, 2, 1);
         INSERT INTO state (key, value) VALUES ('paused', 1);",
    )
    .unwrap();
    drop(old);
}

#[test]
fn a_file_with_the_previous_schema_is_deleted_and_started_again() {
    let dir = data_dir("previous-schema");
    write_previous_schema(&dir);

    let mut db = Community::open(&dir).unwrap();
    assert_eq!(db.next_due(100).unwrap(), None);
    let progress = db.progress(100).unwrap();
    assert_eq!(
        (progress.queued, progress.with_values, progress.paused),
        (0, 0, false)
    );
    // The new file works, and a file of the current shape is kept as it is.
    db.queue(&[3], Source::Library).unwrap();
    db.record_failure(3, "bad", 10, Some(sent(10))).unwrap();
    drop(db);
    let db = Community::open(&dir).unwrap();
    assert_eq!(db.next_due(100).unwrap(), Some(3));
}

/// The schema as the build before whole replies (ADR 0019) wrote it, in its layout: the
/// shape check compares the sql's words, so the spacing has to be the same.
const SCHEMA_BEFORE_WHOLE_REPLIES: &str = "
CREATE TABLE IF NOT EXISTS queue (
    seq INTEGER PRIMARY KEY,
    release_id INTEGER NOT NULL UNIQUE,
    source INTEGER NOT NULL,
    done INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS queue_todo ON queue (source, seq) WHERE done = 0;
CREATE TABLE IF NOT EXISTS result (
    release_id INTEGER PRIMARY KEY REFERENCES queue (release_id),
    fetched_at INTEGER NOT NULL,
    gone INTEGER NOT NULL,
    error TEXT,
    rating_average REAL,
    rating_count INTEGER,
    have INTEGER,
    want INTEGER
);
CREATE INDEX IF NOT EXISTS result_fetched_at ON result (fetched_at) WHERE gone = 0;
CREATE TABLE IF NOT EXISTS retry (
    release_id INTEGER PRIMARY KEY REFERENCES queue (release_id),
    attempts INTEGER NOT NULL,
    retry_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS state (
    key TEXT PRIMARY KEY,
    value NOT NULL
);
";

#[test]
fn a_file_from_before_whole_replies_is_deleted_and_started_again() {
    let dir = data_dir("before-whole-replies");
    let old = rusqlite::Connection::open(dir.join("community.sqlite")).unwrap();
    old.execute_batch(SCHEMA_BEFORE_WHOLE_REPLIES).unwrap();
    old.execute_batch(
        "INSERT INTO queue (release_id, source, done) VALUES (1, 0, 1), (2, 0, 0);
         INSERT INTO result (release_id, fetched_at, gone, rating_average, rating_count, have,
                             want) VALUES (1, 5, 0, 4.0, 3, 2, 1);",
    )
    .unwrap();
    drop(old);
    let db = Community::open(&dir).unwrap();
    let progress = db.progress(100).unwrap();
    assert_eq!((progress.queued, progress.with_values), (0, 0));
}

#[test]
fn a_reply_reads_back_unchanged_until_a_404_drops_it_and_a_failure_keeps_it() {
    let mut db = Community::open(&data_dir("reply")).unwrap();
    db.queue(&[1, 2], Source::Library).unwrap();
    for id in [1, 2] {
        db.record(id, VALUES, REPLY, 0, sent(0)).unwrap();
        assert_eq!(db.reply(id).unwrap().as_deref(), Some(REPLY));
    }
    db.record_gone(1, 10, sent(10)).unwrap();
    assert_eq!(db.reply(1).unwrap(), None);
    // Release 2 fails at a refresh: its earlier reply stays with its earlier values.
    fail_repeatedly(&mut db, 2, 10, REFRESH_AFTER);
    assert_eq!(db.reply(2).unwrap().as_deref(), Some(REPLY));
    assert_eq!(db.progress(REFRESH_AFTER * 2).unwrap().with_values, 1);
    assert_eq!(db.reply(3).unwrap(), None);
}

/// Opens a database of the current shape with a release queued, changes it with `change`
/// through another connection, and opens it again: it is kept as it is, or started again.
fn reopened_after(test: &str, change: &str) -> u64 {
    let dir = data_dir(test);
    Community::open(&dir)
        .unwrap()
        .queue(&[1], Source::Library)
        .unwrap();
    rusqlite::Connection::open(dir.join("community.sqlite"))
        .unwrap()
        .execute_batch(change)
        .unwrap();
    Community::open(&dir).unwrap().progress(0).unwrap().queued
}

#[test]
fn a_difference_in_references_or_indexes_or_tables_starts_the_file_again() {
    // Nothing changed: the queue is kept.
    assert_eq!(reopened_after("shape-same", "SELECT 1"), 1);
    // The same columns without the REFERENCES clause.
    let no_reference = "DROP TABLE retry;
         CREATE TABLE retry (release_id INTEGER PRIMARY KEY, attempts INTEGER NOT NULL,
                             retry_at INTEGER NOT NULL, last_error TEXT NOT NULL)";
    assert_eq!(reopened_after("shape-reference", no_reference), 0);
    // The same index without its WHERE clause.
    let no_where = "DROP INDEX queue_todo; CREATE INDEX queue_todo ON queue (source, seq)";
    assert_eq!(reopened_after("shape-index", no_where), 0);
    // An index the schema doesn't have, and a table it doesn't have.
    let extra_index = "CREATE INDEX extra ON queue (release_id, source)";
    assert_eq!(reopened_after("shape-extra-index", extra_index), 0);
    let extra_table = "CREATE TABLE extra (x)";
    assert_eq!(reopened_after("shape-extra-table", extra_table), 0);
}

#[test]
fn two_threads_opening_an_old_file_at_once_end_with_one_fresh_file() {
    let dir = data_dir("open-race");
    write_previous_schema(&dir);
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        for id in [1, 2] {
            let (dir, barrier) = (&dir, &barrier);
            scope.spawn(move || {
                barrier.wait();
                let mut db = Community::open(dir).unwrap();
                // Both handles work on the same fresh file.
                db.queue(&[id], Source::Library).unwrap();
            });
        }
    });
    let db = Community::open(&dir).unwrap();
    assert_eq!(db.progress(0).unwrap().queued, 2);
}

/// Records `count` bad replies for `id`, each when the one before has waited out its retry
/// time, starting at `start`. Returns the last outcome and the time of the last reply.
fn fail_repeatedly(db: &mut Community, id: u64, count: u64, start: i64) -> (Failure, i64) {
    let mut at = start;
    let mut last = Failure::Retry;
    for _ in 0..count {
        last = db
            .record_failure(id, "Discogs answered 503", at, Some(sent(at)))
            .unwrap()
            .0;
        // Skips ahead to when the release is due again.
        at += 15_360;
    }
    (last, at - 15_360)
}

#[test]
fn a_bad_reply_defers_a_release_a_minute_doubling_for_each_one() {
    let mut db = Community::open(&data_dir("retry-schedule")).unwrap();
    db.queue(&[1, 2], Source::Library).unwrap();
    let mut at = 1000;
    for (attempt, wait) in [60, 120, 240, 480, 960, 1920, 3840, 7680, 15360]
        .into_iter()
        .enumerate()
    {
        let (failure, _) = db
            .record_failure(1, "Discogs answered 503", at, Some(sent(at)))
            .unwrap();
        assert_eq!(failure, Failure::Retry);
        // 2 is never fetched yet, so it goes first while 1 waits; once 2 has values, only
        // 1 is left, and it is due again exactly at its retry time.
        if attempt == 0 {
            assert_eq!(db.next_due(at).unwrap(), Some(2));
            db.record(2, VALUES, REPLY, at, sent(at)).unwrap();
        }
        assert_eq!(
            db.next_due(at + wait - 1).unwrap(),
            None,
            "attempt {attempt}"
        );
        assert_eq!(
            db.next_due(at + wait).unwrap(),
            Some(1),
            "attempt {attempt}"
        );
        at += wait;
    }
    let (failure, message) = db
        .record_failure(1, "Discogs answered 503", at, Some(sent(at)))
        .unwrap();
    assert_eq!(failure, Failure::Failed);
    assert_eq!(message, "release 1: Discogs answered 503");
}

#[test]
fn the_tenth_bad_reply_fails_a_release_until_the_refresh_and_a_fetch_clears_the_count() {
    let mut db = Community::open(&data_dir("retry-failed")).unwrap();
    db.queue(&[1], Source::Library).unwrap();
    let (failure, at) = fail_repeatedly(&mut db, 1, 10, 0);
    assert_eq!(failure, Failure::Failed);
    assert_eq!(db.next_due(at + 10 * DAY).unwrap(), None);
    let progress = db.progress(at).unwrap();
    assert_eq!((progress.failed, progress.queued), (1, 0));
    assert_eq!(db.next_due(at + REFRESH_AFTER).unwrap(), Some(1));

    // A fetch clears the count: nine more problems after five before it don't fail it.
    let mut at = at + REFRESH_AFTER;
    for _ in 0..5 {
        db.record_failure(1, "bad", at, Some(sent(at))).unwrap();
        at += 15_360;
    }
    db.record(1, VALUES, REPLY, at, sent(at)).unwrap();
    for _ in 0..9 {
        let (failure, _) = db.record_failure(1, "bad", at, Some(sent(at))).unwrap();
        assert_eq!(failure, Failure::Retry);
        at += 15_360;
    }
}

#[test]
fn a_deferred_refresh_does_not_hold_the_others() {
    let mut db = Community::open(&data_dir("retry-refresh")).unwrap();
    db.queue(&[1, 2, 3], Source::Library).unwrap();
    db.record(1, VALUES, REPLY, 0, sent(0)).unwrap();
    db.record(2, VALUES, REPLY, DAY, sent(DAY)).unwrap();
    db.record(3, VALUES, REPLY, 2 * DAY, sent(2 * DAY)).unwrap();
    let now = REFRESH_AFTER + 3 * DAY;
    assert_eq!(db.next_due(now).unwrap(), Some(1));
    db.record_failure(1, "Discogs answered 503", now, Some(sent(now)))
        .unwrap();
    assert_eq!(db.next_due(now).unwrap(), Some(2));
    db.record(2, VALUES, REPLY, now, sent(now)).unwrap();
    assert_eq!(db.next_due(now).unwrap(), Some(3));
    db.record(3, VALUES, REPLY, now, sent(now)).unwrap();
    assert_eq!(db.next_due(now).unwrap(), None);
    assert_eq!(db.next_due(now + 60).unwrap(), Some(1));
}

#[test]
fn a_failed_release_with_values_counts_as_with_values_and_the_ten_newest_are_listed() {
    let mut db = Community::open(&data_dir("failed-list")).unwrap();
    let ids: Vec<u64> = (1..=12).collect();
    db.queue(&ids, Source::Library).unwrap();
    db.record(1, VALUES, REPLY, 0, sent(0)).unwrap();
    for &id in &ids {
        // Release 12 fails last.
        let (failure, _) = fail_repeatedly(&mut db, id, 10, id.cast_signed() * 1_000_000);
        assert_eq!(failure, Failure::Failed);
    }
    let progress = db.progress(13_000_000).unwrap();
    assert_eq!((progress.failed, progress.with_values), (12, 1));
    let listed: Vec<u64> = progress.failures.iter().map(|(id, _)| *id).collect();
    assert_eq!(listed, [12, 11, 10, 9, 8, 7, 6, 5, 4, 3]);
    assert_eq!(progress.failures[0].1, "Discogs answered 503");
}

#[test]
fn a_bad_reply_sets_the_last_error_in_the_same_call() {
    let mut db = Community::open(&data_dir("last-error")).unwrap();
    db.queue(&[1], Source::Library).unwrap();
    db.record_failure(1, "Discogs answered 503", 10, Some(sent(10)))
        .unwrap();
    let error = db.progress(10).unwrap().last_error.unwrap();
    assert_eq!(error, (10, "release 1: Discogs answered 503".to_string()));
}

#[test]
fn each_reply_records_its_send_to_the_nanosecond_and_drops_those_kept_too_long() {
    let dir = data_dir("sends");
    let mut db = Community::open(&dir).unwrap();
    db.queue(&[1, 2, 3, 4], Source::Library).unwrap();
    let sent = |seconds, nanos| Sent {
        at: SystemTime::UNIX_EPOCH + Duration::new(seconds, nanos),
        keep: Duration::from_secs(65),
    };
    // Two sends at the same time are two sends.
    db.record(1, VALUES, REPLY, 101, sent(100, 500_000_000))
        .unwrap();
    db.record_gone(2, 101, sent(100, 500_000_000)).unwrap();
    db.record_failure(3, "Discogs answered 410", 102, Some(sent(101, 0)))
        .unwrap();
    db.record_release_error(4, "rate-limited (429)", 102, sent(102, 1))
        .unwrap();
    db.record_error("Discogs refused the token", 103, Some(sent(102, 2)))
        .unwrap();
    // A write that follows another for the same reply records no send.
    db.record_failure(4, "Discogs answered 503", 103, None)
        .unwrap();
    db.record_error("10 replies in a row without values", 103, None)
        .unwrap();
    let recorded = [
        sent(100, 500_000_000),
        sent(100, 500_000_000),
        sent(101, 0),
        sent(102, 1),
        sent(102, 2),
    ];
    let at = |sends: &[Sent]| sends.iter().map(|sent| sent.at).collect::<Vec<_>>();
    assert_eq!(db.recent_sends().unwrap(), at(&recorded));
    // A send `keep` after the first two drops them, and they stay dropped when reopened.
    db.record(1, VALUES, REPLY, 166, sent(165, 500_000_000))
        .unwrap();
    drop(db);
    let db = Community::open(&dir).unwrap();
    let mut kept = recorded[2..].to_vec();
    kept.push(sent(165, 500_000_000));
    assert_eq!(db.recent_sends().unwrap(), at(&kept));
}
