//! The community database's queue order, refresh and reopening.

#[path = "support/dir.rs"]
mod dir;

use std::time::{Duration, SystemTime};

use catalog::community::{
    Community, Counts, Failure, Listed, ListedKind, Progress, Request, RequestKind, Stored, Tier,
    Values,
};
use dir::data_dir;

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

/// Takes every release due at `now`, recording each as fetched then, and returns the ids
/// in the order they came.
fn drain(db: &mut Community, now: i64) -> Vec<u64> {
    let mut order = Vec::new();
    while let Some(request) = db.next_due(now).unwrap() {
        db.record(request.id, VALUES, REPLY, now).unwrap();
        order.push(request.id);
    }
    order
}

const HOUR: i64 = 60 * 60;
const DAY: i64 = 24 * HOUR;

/// Values this old are fetched again.
const REFRESH_AFTER: i64 = catalog::community::REFRESH_AFTER.as_secs().cast_signed();

#[test]
fn takes_the_library_first_then_candidates_each_in_the_order_queued() {
    let mut db = Community::open(&data_dir("community", "order")).unwrap();
    db.queue_releases(&[30, 10], Tier::Ratings).unwrap();
    db.queue_releases(&[5, 20], Tier::Library).unwrap();
    db.queue_releases(&[1], Tier::Ratings).unwrap();
    assert_eq!(drain(&mut db, 1000), [5, 20, 30, 10, 1]);
    assert_eq!(db.next_due(1000).unwrap(), None);
}

/// Artist `id`'s releases, page `page`.
const fn artist(id: u64, page: u64) -> Request {
    Request {
        kind: RequestKind::ArtistReleases,
        id,
        page,
    }
}

/// Master `id`'s versions, page `page`.
const fn master(id: u64, page: u64) -> Request {
    Request {
        kind: RequestKind::MasterVersions,
        id,
        page,
    }
}

/// Takes every request due at `now`, recording a release as fetched and a list as one
/// page of no rows, and returns them in the order they came.
fn drain_requests(db: &mut Community, now: i64) -> Vec<Request> {
    let mut order = Vec::new();
    while let Some(request) = db.next_due(now).unwrap() {
        if request.kind == RequestKind::Release {
            db.record(request.id, VALUES, REPLY, now).unwrap();
        } else {
            db.record_list(request, &[], REPLY, None, now).unwrap();
        }
        order.push(request);
    }
    order
}

#[test]
fn takes_the_library_then_each_tier_by_priority_then_the_order_queued() {
    let mut db = Community::open(&data_dir("community", "tiers")).unwrap();
    let release = Request::release;
    db.queue(&[(release(40), 0.2), (release(41), 0.9)], Tier::Ratings)
        .unwrap();
    db.queue(&[(master(30, 1), 5.0)], Tier::Masters).unwrap();
    db.queue(&[(artist(20, 1), 1.0), (artist(21, 1), 3.0)], Tier::Artists)
        .unwrap();
    db.queue(&[(release(10), 100.0), (release(11), 200.0)], Tier::Asked)
        .unwrap();
    db.queue_releases(&[2, 1], Tier::Library).unwrap();
    // Equal priorities go in the order queued.
    db.queue(&[(release(42), 0.2)], Tier::Ratings).unwrap();
    assert_eq!(
        drain_requests(&mut db, 1000),
        [
            release(2),
            release(1),
            release(11),
            release(10),
            artist(21, 1),
            artist(20, 1),
            master(30, 1),
            release(41),
            release(40),
            release(42),
        ]
    );
}

#[test]
fn queueing_again_rewrites_a_priority_mid_queue_and_never_moves_a_request_down() {
    let mut db = Community::open(&data_dir("community", "rewrite")).unwrap();
    let release = Request::release;
    db.queue(
        &[(release(1), 0.5), (release(2), 0.4), (release(3), 0.3)],
        Tier::Ratings,
    )
    .unwrap();
    assert_eq!(drain_requests(&mut db, 1000).len(), 3);
    db.queue(
        &[(release(4), 0.5), (release(5), 0.4), (release(6), 0.3)],
        Tier::Ratings,
    )
    .unwrap();
    db.record(4, VALUES, REPLY, 1000).unwrap();
    // The scorer between batches: 6 rises past 5, and 5 is asked for, which goes ahead.
    assert_eq!(
        db.queue(&[(release(6), 0.9), (release(5), 0.1)], Tier::Ratings)
            .unwrap(),
        0
    );
    assert_eq!(db.next_due(1000).unwrap(), Some(release(6)));
    db.queue(&[(release(5), 7.0)], Tier::Asked).unwrap();
    assert_eq!(db.next_due(1000).unwrap(), Some(release(5)));
    // Queued as a rating again, an asked request stays asked.
    db.queue(&[(release(5), 0.0)], Tier::Ratings).unwrap();
    db.queue(&[(release(7), 1.0)], Tier::Asked).unwrap();
    assert_eq!(
        drain_requests(&mut db, 1000),
        [release(5), release(7), release(6)]
    );
}

#[test]
fn a_list_s_next_page_is_queued_only_with_the_reply_of_the_page_before() {
    let mut db = Community::open(&data_dir("community", "next-page")).unwrap();
    db.queue(&[(artist(5, 1), 2.0)], Tier::Artists).unwrap();
    db.queue(&[(artist(6, 1), 1.0)], Tier::Artists).unwrap();
    assert_eq!(db.next_due(0).unwrap(), Some(artist(5, 1)));
    assert_eq!(db.progress(0).unwrap().lists.queued, 2);
    let rows = [
        Listed {
            kind: ListedKind::Master,
            id: 900,
            have: 463,
            want: 36,
        },
        Listed {
            kind: ListedKind::Release,
            id: 901,
            have: 51,
            want: 27,
        },
    ];
    db.record_list(artist(5, 1), &rows, REPLY, Some(artist(5, 2)), 10)
        .unwrap();
    // Page 2 has page 1's tier and priority, so it goes before artist 6.
    assert_eq!(db.progress(10).unwrap().lists.queued, 2);
    assert_eq!(db.next_due(10).unwrap(), Some(artist(5, 2)));
    assert_eq!(db.reply(artist(5, 1)).unwrap().as_deref(), Some(REPLY));
    assert_eq!(db.listed(ListedKind::Master, 900).unwrap(), Some(463));
    assert_eq!(db.listed(ListedKind::Release, 901).unwrap(), Some(51));
    assert_eq!(db.listed(ListedKind::Release, 900).unwrap(), None);
    // A list's rows aren't releases with values.
    assert_eq!(db.progress(10).unwrap().candidates, Counts::default());
}

#[test]
fn each_kind_of_request_comes_due_again_after_the_refresh_age() {
    let mut db = Community::open(&data_dir("community", "refresh-kinds")).unwrap();
    db.queue_releases(&[1], Tier::Library).unwrap();
    db.queue(&[(artist(2, 1), 1.0)], Tier::Artists).unwrap();
    db.queue(&[(master(3, 1), 1.0)], Tier::Masters).unwrap();
    db.queue(&[(Request::release(4), 1.0)], Tier::Ratings)
        .unwrap();
    assert_eq!(drain_requests(&mut db, 0).len(), 4);
    assert_eq!(db.next_due(REFRESH_AFTER - 1).unwrap(), None);
    // Fetched at the same time, they come back in the order queued.
    assert_eq!(
        drain_requests(&mut db, REFRESH_AFTER),
        [
            Request::release(1),
            artist(2, 1),
            master(3, 1),
            Request::release(4)
        ]
    );
    assert_eq!(db.progress(REFRESH_AFTER).unwrap().total().due, 0);
}

#[test]
fn queueing_an_id_twice_keeps_one_entry_and_promotes_a_candidate() {
    let mut db = Community::open(&data_dir("community", "twice")).unwrap();
    assert_eq!(db.queue_releases(&[1, 2, 3], Tier::Ratings).unwrap(), 3);
    assert_eq!(db.queue_releases(&[2, 2], Tier::Ratings).unwrap(), 0);
    assert_eq!(db.queue_releases(&[4, 3], Tier::Library).unwrap(), 1);
    // A library id queued again as a candidate stays in the library.
    assert_eq!(db.queue_releases(&[4], Tier::Ratings).unwrap(), 0);
    assert_eq!(drain(&mut db, 1000), [3, 4, 1, 2]);
}

#[test]
fn a_value_comes_due_after_the_refresh_age_behind_anything_never_fetched() {
    let mut db = Community::open(&data_dir("community", "refresh")).unwrap();
    db.queue_releases(&[1], Tier::Library).unwrap();
    db.record(1, VALUES, REPLY, 0).unwrap();
    db.queue_releases(&[2], Tier::Library).unwrap();
    db.record(2, VALUES, REPLY, DAY).unwrap();

    assert_eq!(db.next_due(REFRESH_AFTER - 1).unwrap(), None);
    assert_eq!(db.progress(REFRESH_AFTER - 1).unwrap().total().due, 0);

    // Release 1 is due; a release queued now still goes first.
    db.queue_releases(&[3], Tier::Ratings).unwrap();
    let now = REFRESH_AFTER + DAY;
    assert_eq!(db.progress(now).unwrap().total().due, 2);
    assert_eq!(db.next_due(now).unwrap(), Some(Request::release(3)));
    db.record(3, VALUES, REPLY, now).unwrap();
    // Then the oldest values first.
    assert_eq!(drain(&mut db, now), [1, 2]);
    assert_eq!(db.progress(now).unwrap().total().due, 0);
}

#[test]
fn a_release_answered_404_is_gone_and_not_fetched_again() {
    let mut db = Community::open(&data_dir("community", "gone")).unwrap();
    db.queue_releases(&[1, 2], Tier::Library).unwrap();
    db.record_gone(Request::release(1), 0).unwrap();
    db.record(2, VALUES, REPLY, 0).unwrap();
    let later = REFRESH_AFTER * 2;
    assert_eq!(db.next_due(later).unwrap(), Some(Request::release(2)));
    let progress = db.progress(later).unwrap();
    assert_eq!(
        (
            progress.total().with_values,
            progress.total().gone,
            progress.total().due
        ),
        (1, 1, 1)
    );
}

#[test]
fn reopening_the_file_keeps_the_queue_results_and_state() {
    let dir = data_dir("community", "reopen");
    {
        let mut db = Community::open(&dir).unwrap();
        db.queue_releases(&[1, 2, 3], Tier::Library).unwrap();
        db.record(1, VALUES, REPLY, 50).unwrap();
        db.set_paused(true).unwrap();
        db.record_error("Discogs answered 503", 60).unwrap();
    }
    let db = Community::open(&dir).unwrap();
    assert_eq!(db.next_due(100).unwrap(), Some(Request::release(2)));
    assert_eq!(
        db.progress(100).unwrap(),
        Progress {
            library: Counts {
                queued: 2,
                with_values: 1,
                ..Counts::default()
            },
            candidates: Counts::default(),
            lists: Counts::default(),
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
    let dir = data_dir("community", "missing").join("absent");
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
    let dir = data_dir("community", "previous-schema");
    write_previous_schema(&dir);

    let mut db = Community::open(&dir).unwrap();
    assert_eq!(db.next_due(100).unwrap(), None);
    let progress = db.progress(100).unwrap();
    assert_eq!(
        (
            progress.total().queued,
            progress.total().with_values,
            progress.paused
        ),
        (0, 0, false)
    );
    // The new file works, and a file of the current shape is kept as it is.
    db.queue_releases(&[3], Tier::Library).unwrap();
    db.record_failure(Request::release(3), "bad", 10).unwrap();
    drop(db);
    let db = Community::open(&dir).unwrap();
    assert_eq!(db.next_due(100).unwrap(), Some(Request::release(3)));
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
    let dir = data_dir("community", "before-whole-replies");
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
    assert_eq!(
        (progress.total().queued, progress.total().with_values),
        (0, 0)
    );
}

#[test]
fn a_reply_reads_back_unchanged_until_a_404_drops_it_and_a_failure_keeps_it() {
    let mut db = Community::open(&data_dir("community", "reply")).unwrap();
    db.queue_releases(&[1, 2], Tier::Library).unwrap();
    for id in [1, 2] {
        db.record(id, VALUES, REPLY, 0).unwrap();
        assert_eq!(
            db.reply(Request::release(id)).unwrap().as_deref(),
            Some(REPLY)
        );
    }
    db.record_gone(Request::release(1), 10).unwrap();
    assert_eq!(db.reply(Request::release(1)).unwrap(), None);
    // Release 2 fails at a refresh: its earlier reply stays with its earlier values.
    fail_repeatedly(&mut db, 2, 10, REFRESH_AFTER);
    assert_eq!(
        db.reply(Request::release(2)).unwrap().as_deref(),
        Some(REPLY)
    );
    assert_eq!(
        db.progress(REFRESH_AFTER * 2).unwrap().total().with_values,
        1
    );
    assert_eq!(db.reply(Request::release(3)).unwrap(), None);
}

/// Opens a database of the current shape with a release queued, changes it with `change`
/// through another connection, and opens it again: it is kept as it is, or started again.
fn reopened_after(test: &str, change: &str) -> u64 {
    let dir = data_dir("community", test);
    Community::open(&dir)
        .unwrap()
        .queue_releases(&[1], Tier::Library)
        .unwrap();
    rusqlite::Connection::open(dir.join("community.sqlite"))
        .unwrap()
        .execute_batch(change)
        .unwrap();
    Community::open(&dir)
        .unwrap()
        .progress(0)
        .unwrap()
        .total()
        .queued
}

#[test]
fn a_difference_in_references_or_indexes_or_tables_starts_the_file_again() {
    // Nothing changed: the queue is kept.
    assert_eq!(reopened_after("shape-same", "SELECT 1"), 1);
    // The same columns without the REFERENCES clause.
    let no_reference = "DROP TABLE retry;
         CREATE TABLE retry (request INTEGER PRIMARY KEY, attempts INTEGER NOT NULL,
                             retry_at INTEGER NOT NULL)";
    assert_eq!(reopened_after("shape-reference", no_reference), 0);
    // The same index without its WHERE clause.
    let no_where = "DROP INDEX request_todo;
         CREATE INDEX request_todo ON request (tier, priority DESC, seq)";
    assert_eq!(reopened_after("shape-index", no_where), 0);
    // An index the schema doesn't have, and a table it doesn't have.
    let extra_index = "CREATE INDEX extra ON request (id, tier)";
    assert_eq!(reopened_after("shape-extra-index", extra_index), 0);
    let extra_table = "CREATE TABLE extra (x)";
    assert_eq!(reopened_after("shape-extra-table", extra_table), 0);
}

#[test]
fn two_threads_opening_an_old_file_at_once_end_with_one_fresh_file() {
    let dir = data_dir("community", "open-race");
    write_previous_schema(&dir);
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        for id in [1, 2] {
            let (dir, barrier) = (&dir, &barrier);
            scope.spawn(move || {
                barrier.wait();
                let mut db = Community::open(dir).unwrap();
                // Both handles work on the same fresh file.
                db.queue_releases(&[id], Tier::Library).unwrap();
            });
        }
    });
    let db = Community::open(&dir).unwrap();
    assert_eq!(db.progress(0).unwrap().total().queued, 2);
}

/// Records `count` bad replies for `id`, each when the one before has waited out its retry
/// time, starting at `start`. Returns the last outcome and the time of the last reply.
fn fail_repeatedly(db: &mut Community, id: u64, count: u64, start: i64) -> (Failure, i64) {
    let mut at = start;
    let mut last = Failure::Retry;
    for _ in 0..count {
        last = db
            .record_failure(Request::release(id), "Discogs answered 503", at)
            .unwrap()
            .0;
        // Skips ahead to when the release is due again.
        at += 15_360;
    }
    (last, at - 15_360)
}

#[test]
fn a_bad_reply_defers_a_release_a_minute_doubling_for_each_one() {
    let mut db = Community::open(&data_dir("community", "retry-schedule")).unwrap();
    db.queue_releases(&[1, 2], Tier::Library).unwrap();
    let mut at = 1000;
    for (attempt, wait) in [60, 120, 240, 480, 960, 1920, 3840, 7680, 15360]
        .into_iter()
        .enumerate()
    {
        let (failure, _) = db
            .record_failure(Request::release(1), "Discogs answered 503", at)
            .unwrap();
        assert_eq!(failure, Failure::Retry);
        // 2 is never fetched yet, so it goes first while 1 waits; once 2 has values, only
        // 1 is left, and it is due again exactly at its retry time.
        if attempt == 0 {
            assert_eq!(db.next_due(at).unwrap(), Some(Request::release(2)));
            db.record(2, VALUES, REPLY, at).unwrap();
        }
        assert_eq!(
            db.next_due(at + wait - 1).unwrap(),
            None,
            "attempt {attempt}"
        );
        assert_eq!(
            db.next_due(at + wait).unwrap(),
            Some(Request::release(1)),
            "attempt {attempt}"
        );
        at += wait;
    }
    let (failure, message) = db
        .record_failure(Request::release(1), "Discogs answered 503", at)
        .unwrap();
    assert_eq!(failure, Failure::Failed);
    assert_eq!(message, "release 1: Discogs answered 503");
}

#[test]
fn the_tenth_bad_reply_fails_a_release_until_the_refresh_and_a_fetch_clears_the_count() {
    let mut db = Community::open(&data_dir("community", "retry-failed")).unwrap();
    db.queue_releases(&[1], Tier::Library).unwrap();
    let (failure, at) = fail_repeatedly(&mut db, 1, 10, 0);
    assert_eq!(failure, Failure::Failed);
    assert_eq!(db.next_due(at + 10 * DAY).unwrap(), None);
    let progress = db.progress(at).unwrap();
    assert_eq!((progress.total().failed, progress.total().queued), (1, 0));
    assert_eq!(
        db.next_due(at + REFRESH_AFTER).unwrap(),
        Some(Request::release(1))
    );

    // A fetch clears the count: nine more problems after five before it don't fail it.
    let mut at = at + REFRESH_AFTER;
    for _ in 0..5 {
        db.record_failure(Request::release(1), "bad", at).unwrap();
        at += 15_360;
    }
    db.record(1, VALUES, REPLY, at).unwrap();
    for _ in 0..9 {
        let (failure, _) = db.record_failure(Request::release(1), "bad", at).unwrap();
        assert_eq!(failure, Failure::Retry);
        at += 15_360;
    }
}

#[test]
fn a_deferred_refresh_does_not_hold_the_others() {
    let mut db = Community::open(&data_dir("community", "retry-refresh")).unwrap();
    db.queue_releases(&[1, 2, 3], Tier::Library).unwrap();
    db.record(1, VALUES, REPLY, 0).unwrap();
    db.record(2, VALUES, REPLY, DAY).unwrap();
    db.record(3, VALUES, REPLY, 2 * DAY).unwrap();
    let now = REFRESH_AFTER + 3 * DAY;
    assert_eq!(db.next_due(now).unwrap(), Some(Request::release(1)));
    db.record_failure(Request::release(1), "Discogs answered 503", now)
        .unwrap();
    assert_eq!(db.next_due(now).unwrap(), Some(Request::release(2)));
    db.record(2, VALUES, REPLY, now).unwrap();
    assert_eq!(db.next_due(now).unwrap(), Some(Request::release(3)));
    db.record(3, VALUES, REPLY, now).unwrap();
    assert_eq!(db.next_due(now).unwrap(), None);
    assert_eq!(db.next_due(now + 60).unwrap(), Some(Request::release(1)));
}

#[test]
fn a_failed_release_with_values_counts_as_with_values_and_the_ten_newest_are_listed() {
    let mut db = Community::open(&data_dir("community", "failed-list")).unwrap();
    let ids: Vec<u64> = (1..=12).collect();
    db.queue_releases(&ids, Tier::Library).unwrap();
    db.record(1, VALUES, REPLY, 0).unwrap();
    for &id in &ids {
        // Release 12 fails last.
        let (failure, _) = fail_repeatedly(&mut db, id, 10, id.cast_signed() * 1_000_000);
        assert_eq!(failure, Failure::Failed);
    }
    let progress = db.progress(13_000_000).unwrap();
    assert_eq!(
        (progress.total().failed, progress.total().with_values),
        (12, 1)
    );
    let listed: Vec<u64> = progress.failures.iter().map(|(id, _)| *id).collect();
    assert_eq!(listed, [12, 11, 10, 9, 8, 7, 6, 5, 4, 3]);
    assert_eq!(progress.failures[0].1, "Discogs answered 503");
}

#[test]
fn a_bad_reply_sets_the_last_error_in_the_same_call() {
    let mut db = Community::open(&data_dir("community", "last-error")).unwrap();
    db.queue_releases(&[1], Tier::Library).unwrap();
    db.record_failure(Request::release(1), "Discogs answered 503", 10)
        .unwrap();
    let error = db.progress(10).unwrap().last_error.unwrap();
    assert_eq!(error, (10, "release 1: Discogs answered 503".to_string()));
}

#[test]
fn each_send_is_recorded_to_the_nanosecond_and_those_kept_too_long_are_dropped() {
    let dir = data_dir("community", "sends");
    let mut db = Community::open(&dir).unwrap();
    let keep = Duration::from_secs(65);
    let at = |seconds, nanos| SystemTime::UNIX_EPOCH + Duration::new(seconds, nanos);
    // Two sends at the same time are two sends.
    let recorded = [
        at(100, 500_000_000),
        at(100, 500_000_000),
        at(101, 0),
        at(102, 1),
        at(102, 2),
    ];
    for send in recorded {
        db.record_send(send, keep).unwrap();
    }
    assert_eq!(db.recent_sends(SystemTime::UNIX_EPOCH).unwrap(), recorded);
    // A send `keep` after the first two drops them, and they stay dropped when reopened.
    db.record_send(at(165, 500_000_000), keep).unwrap();
    drop(db);
    let db = Community::open(&dir).unwrap();
    let mut kept = recorded[2..].to_vec();
    kept.push(at(165, 500_000_000));
    assert_eq!(db.recent_sends(SystemTime::UNIX_EPOCH).unwrap(), kept);
}

#[test]
fn reads_and_counts_only_the_sends_after_a_time() {
    let mut db = Community::open(&data_dir("community", "sends-after")).unwrap();
    let at = |seconds| SystemTime::UNIX_EPOCH + Duration::from_secs(seconds);
    for sent in [100, 200, 300] {
        db.record_send(at(sent), Duration::from_hours(1)).unwrap();
    }
    // A send at the time given is not after it.
    assert_eq!(db.recent_sends(at(200)).unwrap(), [at(300)]);
    assert_eq!(db.sends_after(at(200)).unwrap(), 1);
    assert_eq!(db.sends_after(at(99)).unwrap(), 3);
    assert_eq!(db.sends_after(at(300)).unwrap(), 0);
}

#[test]
fn stored_reads_each_ids_values_or_why_it_has_none_in_the_order_asked() {
    let mut db = Community::open(&data_dir("community", "stored")).unwrap();
    db.queue_releases(&[1, 2, 3, 4, 5], Tier::Library).unwrap();
    db.record(1, VALUES, REPLY, 100).unwrap();
    db.record_gone(Request::release(2), 200).unwrap();
    // Release 3 fetched once, then failed ten times on its refresh: its values stay.
    db.record(3, VALUES, REPLY, 300).unwrap();
    // Release 4 failed ten times and was never fetched.
    for attempt in 0..10 {
        db.record_failure(Request::release(3), "a release problem", 400 + attempt)
            .unwrap();
        db.record_failure(Request::release(4), "a release problem", 500 + attempt)
            .unwrap();
    }
    // Release 5 is queued and has one problem on record, so it waits to be tried again.
    db.record_failure(Request::release(5), "a release problem", 600)
        .unwrap();
    assert_eq!(
        db.stored(&[5, 4, 3, 2, 1, 99]).unwrap(),
        [
            Stored::Queued,
            Stored::Failed {
                values: None,
                at: 509
            },
            Stored::Failed {
                values: Some(VALUES),
                at: 409
            },
            Stored::Gone { at: 200 },
            Stored::Fetched {
                values: VALUES,
                at: 100
            },
            Stored::NotQueued,
        ]
    );
    assert_eq!(db.stored(&[]).unwrap(), []);
}
