//! The scores database: a list written whole and renamed in, read back in rank order with
//! its reasons and what it was computed from; a write that stops before the rename leaves
//! the list in use as it was.

#[path = "support/dir.rs"]
mod dir;

use catalog::discogs::Reading;
use catalog::feedback::Kind;
use catalog::score::{
    ArtistRow, Computed, Inputs, Item, Named, Reason, ReasonKind, Relation, Row, Scores, Writer,
    digest,
};
use dir::data_dir;

fn named(id: u64, name: &str) -> Named {
    Named {
        id,
        name: Some(name.to_string()),
    }
}

/// A reason of `kind` with nothing but its weight.
fn reason(kind: ReasonKind, weight: f64) -> Reason {
    Reason {
        weight,
        kind,
        named: None,
        role: None,
        other: None,
        relation: None,
        albums: None,
        items: Vec::new(),
    }
}

/// Two candidates with a reason of each kind between them.
fn rows() -> Vec<Row> {
    vec![
        Row {
            kind: Kind::Master,
            id: 7,
            representative: Some(2002),
            score: 1.0,
            reasons: vec![
                Reason {
                    named: Some(named(21, "Dee Producer")),
                    role: Some("Producer".to_string()),
                    albums: Some(2),
                    items: vec![Item {
                        id: None,
                        name: Some("Mixed By".to_string()),
                    }],
                    ..reason(ReasonKind::Credit, 1.5)
                },
                Reason {
                    named: Some(named(40, "Sub Records")),
                    other: Some(named(30, "Seed Records")),
                    albums: Some(1),
                    ..reason(ReasonKind::Label, 1.2)
                },
                Reason {
                    items: vec![
                        Item {
                            id: Some(3),
                            name: Some("Made-Up Rare".to_string()),
                        },
                        Item {
                            id: Some(4),
                            name: None,
                        },
                    ],
                    ..reason(ReasonKind::Styles, 0.25)
                },
            ],
        },
        Row {
            kind: Kind::Release,
            id: 2010,
            representative: None,
            score: 0.5,
            reasons: vec![
                Reason {
                    named: Some(named(11, "DJ Ann")),
                    other: Some(named(10, "Ann Example")),
                    relation: Some(Relation::Alias),
                    ..reason(ReasonKind::Related, 0.75)
                },
                Reason {
                    named: Some(Named { id: 20, name: None }),
                    albums: Some(1),
                    ..reason(ReasonKind::Artist, 0.5)
                },
            ],
        },
    ]
}

fn inputs(profile: &str) -> Inputs {
    Inputs {
        catalog_set: "20000101".to_string(),
        catalog_swapped_at: 100,
        profile_digest: digest(profile),
        scoring_version: 1,
    }
}

/// Writes `rows` computed from `inputs` with `seeds` seeds at `now`, and renames them in.
fn write(data_dir: &std::path::Path, rows: &[Row], inputs: &Inputs, now: i64) {
    let mut writer = Writer::create(data_dir).unwrap();
    for row in rows {
        writer.add(row).unwrap();
    }
    writer.finish(inputs, 3, now).unwrap();
}

fn open(data_dir: &std::path::Path) -> Scores {
    match Scores::open_read_only(data_dir).unwrap() {
        Reading::Open(scores) => scores,
        Reading::Missing => panic!("no scores"),
        Reading::OtherShape => panic!("scores of another shape"),
    }
}

#[test]
fn a_list_written_reads_back_in_rank_order_with_its_reasons() {
    let dir = data_dir("score", "written");
    assert!(matches!(
        Scores::open_read_only(&dir).unwrap(),
        Reading::Missing
    ));
    write(&dir, &rows(), &inputs("[]"), 200);
    let scores = open(&dir);
    assert_eq!(
        scores.computed().unwrap(),
        Computed {
            inputs: inputs("[]"),
            seeds: 3,
            candidates: 2,
            finished_at: 200,
        }
    );
    assert_eq!(scores.page(0, 10).unwrap(), rows());
    assert_eq!(scores.page(0, 1).unwrap(), rows()[..1]);
    // A page reads the ranks after `after`, and nothing past the end.
    assert_eq!(scores.page(1, 10).unwrap(), rows()[1..]);
    assert_eq!(scores.page(0, 1).unwrap(), rows()[..1]);
    assert_eq!(scores.page(2, 10).unwrap(), rows()[2..]);
    assert_eq!(scores.page(u64::MAX, u64::MAX).unwrap(), rows()[2..]);
    assert!(!dir.join("scores.next.sqlite").exists());
}

#[test]
fn a_write_that_stops_before_the_rename_leaves_the_list_in_use() {
    let dir = data_dir("score", "interrupted");
    write(&dir, &rows(), &inputs("old"), 200);
    let mut writer = Writer::create(&dir).unwrap();
    writer.add(&rows()[1]).unwrap();
    drop(writer);
    let scores = open(&dir);
    assert_eq!(scores.computed().unwrap().inputs, inputs("old"));
    assert_eq!(scores.page(0, 10).unwrap(), rows());
    drop(scores);
    // The next write starts again from nothing.
    write(&dir, &rows()[1..], &inputs("new"), 300);
    let scores = open(&dir);
    assert_eq!(scores.computed().unwrap().inputs, inputs("new"));
    assert_eq!(scores.page(0, 10).unwrap(), rows()[1..]);
}

#[test]
fn the_digest_is_fixed_in_size_and_in_value() {
    // FNV-1a and FNV-1 of "a" and of nothing, from the algorithms' published vectors: a
    // stored digest has to match the one a later build computes.
    assert_eq!(digest("a"), "af63dc4c8601ec8caf63bd4c8601b7be");
    assert_eq!(digest(""), "cbf29ce484222325cbf29ce484222325");
    assert_ne!(digest("[1,2]"), digest("[2,1]"));
    assert_eq!(digest(&"x".repeat(1_000_000)).len(), 32);
}

#[test]
fn a_file_that_stores_the_profile_whole_is_read_as_another_shape() {
    let dir = data_dir("score", "old-profile-column");
    write(&dir, &rows(), &inputs("[]"), 200);
    let connection = rusqlite::Connection::open(dir.join("scores.sqlite")).unwrap();
    connection
        .execute_batch("ALTER TABLE computed RENAME COLUMN profile_digest TO profile")
        .unwrap();
    drop(connection);
    assert!(matches!(
        Scores::open_read_only(&dir).unwrap(),
        Reading::OtherShape
    ));
}

#[test]
fn a_file_of_another_shape_is_read_as_one() {
    let dir = data_dir("score", "other-shape");
    let connection = rusqlite::Connection::open(dir.join("scores.sqlite")).unwrap();
    connection
        .execute_batch("CREATE TABLE candidate (rank INTEGER PRIMARY KEY)")
        .unwrap();
    drop(connection);
    assert!(matches!(
        Scores::open_read_only(&dir).unwrap(),
        Reading::OtherShape
    ));
    write(&dir, &rows(), &inputs("[]"), 200);
    assert_eq!(open(&dir).page(0, 10).unwrap(), rows());
}

fn artists() -> Vec<ArtistRow> {
    vec![
        ArtistRow {
            id: 7,
            name: Some("Seven".to_string()),
            score: 1.0,
            albums: 2,
            best_rank: 2,
        },
        ArtistRow {
            id: 3,
            name: None,
            score: 0.5,
            albums: 1,
            best_rank: 1,
        },
    ]
}

#[test]
fn the_artists_read_back_best_first_and_a_candidate_by_its_rank() {
    let dir = data_dir("score", "artists");
    let mut writer = Writer::create(&dir).unwrap();
    for row in rows() {
        writer.add(&row).unwrap();
    }
    for artist in artists() {
        writer.add_artist(&artist).unwrap();
    }
    writer.finish(&inputs("[]"), 3, 200).unwrap();
    let scores = open(&dir);
    assert_eq!(scores.artists(10).unwrap(), artists());
    assert_eq!(scores.artists(1).unwrap(), artists()[..1]);
    assert_eq!(scores.candidate(2).unwrap(), Some(rows()[1].clone()));
    assert_eq!(scores.candidate(1).unwrap(), Some(rows()[0].clone()));
    assert_eq!(scores.candidate(3).unwrap(), None);
}

#[test]
fn a_file_without_the_artists_is_read_as_another_shape() {
    let dir = data_dir("score", "no-artists");
    write(&dir, &rows(), &inputs("[]"), 200);
    let connection = rusqlite::Connection::open(dir.join("scores.sqlite")).unwrap();
    connection.execute_batch("DROP TABLE artist").unwrap();
    drop(connection);
    assert!(matches!(
        Scores::open_read_only(&dir).unwrap(),
        Reading::OtherShape
    ));
}
