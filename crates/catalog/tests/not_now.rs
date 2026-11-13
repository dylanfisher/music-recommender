//! Rediscover's not-now database: a mark per album that lasts until its end, replaced when
//! set again, undone, and kept in a file of its own beside the feedback's.

#[path = "support/dir.rs"]
mod dir;

use catalog::feedback::{Feedback, Kind, Verdict};
use catalog::not_now::NotNow;
use dir::data_dir;

#[test]
fn a_mark_lasts_until_its_end_and_can_be_replaced_or_undone() {
    let mut db = NotNow::open(&data_dir("not_now", "marks")).unwrap();
    db.set("r4242", 1000, 100).unwrap();
    db.set("k0123456789abcdef", 500, 100).unwrap();
    assert_eq!(db.marked(100).unwrap(), ["k0123456789abcdef", "r4242"]);
    // A mark ends at its `until`.
    assert_eq!(db.marked(500).unwrap(), ["r4242"]);
    // Sent again, it replaces the one there, and removes the marks ended.
    db.set("r4242", 300, 600).unwrap();
    assert_eq!(db.marked(299).unwrap(), ["r4242"]);
    assert_eq!(db.marked(300).unwrap(), Vec::<String>::new());
    assert!(db.undo("r4242").unwrap());
    assert!(!db.undo("r4242").unwrap());
}

#[test]
fn the_marks_and_the_feedback_are_two_files() {
    let dir = data_dir("not_now", "two-files");
    let mut feedback = Feedback::open(&dir).unwrap();
    feedback
        .set(Kind::Release, 4242, Verdict::NotInterested, 100)
        .unwrap();
    NotNow::open(&dir).unwrap().set("r4242", 1000, 100).unwrap();
    assert_eq!(feedback.list(0).unwrap().marks.len(), 1);
    assert!(dir.join("rediscover.sqlite").exists());
}
