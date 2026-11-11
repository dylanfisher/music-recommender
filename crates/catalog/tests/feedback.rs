//! The feedback database: one verdict per release or master, undone, paged by `seq`, and kept
//! apart from the community database, whose reset doesn't touch it.

#[path = "support/dir.rs"]
mod dir;

use catalog::community::Community;
use catalog::feedback::{Feedback, Kind, Mark, Verdict};
use dir::data_dir;

/// The most marks `list` returns at once (docs/api.md, Feedback).
const PAGE: usize = 500;

fn mark(seq: i64, kind: Kind, id: i64, verdict: Verdict, at: i64) -> Mark {
    Mark {
        seq,
        kind,
        id,
        verdict,
        at,
    }
}

#[test]
fn keeps_one_verdict_per_release_or_master_and_lists_them_in_the_order_set() {
    let mut db = Feedback::open(&data_dir("feedback", "set")).unwrap();
    let first = db
        .set(Kind::Release, 4242, Verdict::NotInterested, 100)
        .unwrap();
    assert_eq!(
        first,
        mark(1, Kind::Release, 4242, Verdict::NotInterested, 100)
    );
    // A master and a release with the same id are two things.
    db.set(Kind::Master, 4242, Verdict::AlreadyHave, 200)
        .unwrap();
    // The same verdict again leaves it as it was.
    assert_eq!(
        db.set(Kind::Release, 4242, Verdict::NotInterested, 300)
            .unwrap(),
        first
    );
    // Another verdict replaces it, and it moves to the end.
    let changed = db
        .set(Kind::Release, 4242, Verdict::AlreadyHave, 400)
        .unwrap();
    assert_eq!(
        changed,
        mark(3, Kind::Release, 4242, Verdict::AlreadyHave, 400)
    );
    let page = db.list(0).unwrap();
    assert_eq!(
        page.marks,
        [
            mark(2, Kind::Master, 4242, Verdict::AlreadyHave, 200),
            changed
        ]
    );
    assert!(!page.more);
    assert_eq!(db.list(2).unwrap().marks, [changed]);
    assert_eq!(db.list(3).unwrap().marks, []);
}

#[test]
fn undo_removes_a_verdict_and_its_seq_is_never_given_again() {
    let mut db = Feedback::open(&data_dir("feedback", "undo")).unwrap();
    db.set(Kind::Release, 1, Verdict::NotInterested, 100)
        .unwrap();
    db.set(Kind::Release, 2, Verdict::NotInterested, 100)
        .unwrap();
    assert!(db.undo(Kind::Release, 2).unwrap());
    assert!(!db.undo(Kind::Release, 2).unwrap());
    assert!(!db.undo(Kind::Master, 1).unwrap());
    // A client that paged past seq 2 still sees what comes after it.
    let again = db.set(Kind::Release, 2, Verdict::AlreadyHave, 200).unwrap();
    assert_eq!(again.seq, 3);
    assert_eq!(db.list(2).unwrap().marks, [again]);
}

#[test]
fn pages_at_most_a_page_at_a_time() {
    let mut db = Feedback::open(&data_dir("feedback", "pages")).unwrap();
    let count = i64::try_from(PAGE).unwrap() + 1;
    for id in 1..=count {
        db.set(Kind::Release, id, Verdict::NotInterested, id)
            .unwrap();
    }
    let first = db.list(0).unwrap();
    assert_eq!(first.marks.len(), PAGE);
    assert!(first.more);
    let last = first.marks.last().unwrap().seq;
    let second = db.list(last).unwrap();
    assert_eq!(second.marks.len(), 1);
    assert_eq!(second.marks[0].id, count);
    assert!(!second.more);
}

#[test]
fn starting_the_community_database_again_keeps_the_feedback() {
    let dir = data_dir("feedback", "apart");
    let mut db = Feedback::open(&dir).unwrap();
    let kept = db
        .set(Kind::Master, 7, Verdict::NotInterested, 100)
        .unwrap();
    drop(db);
    // A community database of another shape is deleted and started again when opened.
    rusqlite::Connection::open(dir.join("community.sqlite"))
        .unwrap()
        .execute_batch("CREATE TABLE queue (release_id INTEGER);")
        .unwrap();
    Community::open(&dir).unwrap();
    assert_eq!(Feedback::open(&dir).unwrap().list(0).unwrap().marks, [kept]);
}
