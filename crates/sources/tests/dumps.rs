//! Each dump's parser on the hand-written fixture set in `fixtures/dumps/` (a few records
//! each, in the shape of Discogs's files), and the truncated copy of its releases dump.

use std::path::PathBuf;

use sources::discogs::artist::Artist;
use sources::discogs::dump::{Record, Stream};
use sources::discogs::label::Label;
use sources::discogs::master::Master;
use sources::discogs::release::{Credit, Format, Release, ReleaseLabel};
use sources::discogs::set::{self, Dump, Set};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn fixture_set() -> Set {
    set::newest_complete(&fixtures().join("dumps"))
        .unwrap()
        .unwrap()
}

/// Every record of `dump` in the fixture set, read to the end, so its checksum matched.
fn read<R: Record>(dump: Dump) -> Vec<R> {
    let set = fixture_set();
    let mut stream = Stream::<R>::open(&set.path(dump), set.sha256(dump)).unwrap();
    let mut records = Vec::new();
    while let Some(record) = stream.read().unwrap() {
        records.push(record);
    }
    assert_eq!(stream.records(), records.len() as u64);
    records
}

fn strings(list: &[&str]) -> Vec<String> {
    list.iter().map(ToString::to_string).collect()
}

#[test]
fn the_fixture_set_is_complete() {
    assert_eq!(fixture_set().date, "20000101");
}

#[test]
fn artists_keep_names_aliases_and_both_sides_of_a_group() {
    let artists = read::<Artist>(Dump::Artists);
    assert_eq!(
        artists.iter().map(|a| a.id).collect::<Vec<_>>(),
        [1, 2, 3, 4, 5, 20, 21, 22, 23, 194]
    );
    let band = &artists[0];
    assert_eq!(band.name, "The Made-Up Band");
    assert_eq!(band.realname, None);
    assert_eq!(band.namevariations, strings(&["Made-Up Band", "TMUB"]));
    assert_eq!(band.members, [2, 3]);
    assert!(band.groups.is_empty() && band.aliases.is_empty());
    let ann = &artists[1];
    assert_eq!(ann.realname.as_deref(), Some("Ann Q. Example"));
    assert_eq!(ann.aliases, [4]);
    assert_eq!(ann.groups, [1]);
    assert_eq!(artists[4].name, "Lone & Only");
}

#[test]
fn labels_keep_their_parent_and_not_their_sublabels() {
    let labels = read::<Label>(Dump::Labels);
    assert_eq!(
        labels
            .iter()
            .map(|l| (l.id, l.name.as_str(), l.parent_id))
            .collect::<Vec<_>>(),
        [
            (10, "Example Records", None),
            (11, "Example Sub", Some(10)),
            (12, "Other Label", None),
            (30, "Seed Records", None),
            (31, "Seed Sub", Some(30)),
        ]
    );
}

#[test]
fn masters_keep_their_main_release_artists_and_styles() {
    let masters = read::<Master>(Dump::Masters);
    assert_eq!(
        masters[0],
        Master {
            id: 100,
            main_release: 1001,
            title: "First Album".to_string(),
            year: Some(1999),
            artists: vec![1],
            genres: strings(&["Rock"]),
            styles: strings(&["Indie Rock", "Shoegaze"]),
        }
    );
    // Year 0 is Discogs's unknown year.
    assert_eq!(masters[1].year, None);
    assert_eq!(masters[1].artists, [2, 4]);
}

#[test]
fn a_release_keeps_the_fields_the_catalog_uses() {
    let releases = read::<Release>(Dump::Releases);
    let credit = |artist_id, role: &str| Credit {
        artist_id,
        role: role.to_string(),
    };
    assert_eq!(
        releases[0],
        Release {
            id: 1001,
            title: "First Album".to_string(),
            master_id: 100,
            is_main_release: true,
            released: Some("1999-03-00".to_string()),
            year: Some(1999),
            country: Some("UK".to_string()),
            artists: vec![1],
            labels: vec![
                ReleaseLabel {
                    label_id: 10,
                    catno: Some("EX 001".to_string()),
                },
                ReleaseLabel {
                    label_id: 11,
                    catno: Some("none".to_string()),
                },
            ],
            // Release-level and track-level, sub-tracks' too, with the bracketed scopes
            // dropped, one of each pair.
            credits: vec![
                credit(2, "Producer"),
                credit(2, "Written-By"),
                credit(3, "Lacquer Cut By"),
                credit(3, "Performer"),
                credit(3, "Producer"),
                credit(5, "Strings"),
            ],
            formats: vec![
                Format {
                    name: "Vinyl".to_string(),
                    qty: 2,
                    descriptions: strings(&["LP", "Album"]),
                },
                Format {
                    name: "CD".to_string(),
                    qty: 1,
                    descriptions: strings(&["Album"]),
                },
            ],
            genres: strings(&["Rock", "Electronic"]),
            styles: strings(&["Indie Rock", "Shoegaze"]),
            barcodes: strings(&["5 012345 678900", "5012345678900"]),
            // The heading has no position and isn't a track; the sub-track isn't counted.
            durations: vec![285, 0, 3723],
        }
    );
    assert_eq!(releases[1].master_id, 100);
    assert!(!releases[1].is_main_release);
    assert_eq!(releases[1].year, Some(2004));
}

#[test]
fn a_release_with_every_optional_element_missing() {
    let releases = read::<Release>(Dump::Releases);
    assert_eq!(
        releases[2],
        Release {
            id: 1003,
            title: "Untitled".to_string(),
            master_id: 0,
            is_main_release: false,
            released: None,
            year: None,
            country: None,
            artists: vec![2],
            labels: vec![],
            credits: vec![],
            formats: vec![],
            genres: vec![],
            styles: vec![],
            barcodes: vec![],
            durations: vec![0],
        }
    );
}

#[test]
fn the_truncated_releases_dump_fails_after_the_records_before_the_cut() {
    let set = fixture_set();
    let path = fixtures()
        .join("dumps-truncated")
        .join(set::file_name(&set.date, Dump::Releases));
    let mut stream = Stream::<Release>::open(&path, set.sha256(Dump::Releases)).unwrap();
    assert_eq!(stream.read().unwrap().unwrap().id, 1001);
    assert_eq!(stream.read().unwrap().unwrap().id, 1002);
    let error = stream.read().unwrap_err().to_string();
    assert!(error.contains("truncated"), "{error}");
}

#[test]
fn a_dump_read_against_another_dumps_checksum_fails() {
    let set = fixture_set();
    let mut stream =
        Stream::<Label>::open(&set.path(Dump::Labels), set.sha256(Dump::Artists)).unwrap();
    let error = loop {
        match stream.read() {
            Ok(Some(_)) => {}
            Ok(None) => panic!("the checksum matched"),
            Err(error) => break error.to_string(),
        }
    };
    assert!(error.contains(set.sha256(Dump::Artists)), "{error}");
}
