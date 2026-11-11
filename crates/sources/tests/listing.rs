//! Reading data.discogs.com's listing of a year's dumps, on a trimmed copy of the real 2026
//! page.

use sources::discogs::listing;

const YEAR_2026: &str = include_str!("fixtures/listing/2026.html");

#[test]
fn reads_each_file_and_its_size_from_a_years_listing() {
    let entries = listing::parse(YEAR_2026);
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names.len(), 10, "{names:?}");
    assert_eq!(names[0], "discogs_20260101_CHECKSUM.txt");
    assert_eq!(names[9], "discogs_20261101_releases.xml.gz");
    let size = |name: &str| &entries.iter().find(|e| e.name == name).unwrap().size;
    // The sizes of the files of 2026-11-01 on the NAS, which the listing rounds.
    assert!(size("discogs_20261101_CHECKSUM.txt").matches(388));
    assert!(!size("discogs_20261101_CHECKSUM.txt").matches(389));
    assert!(size("discogs_20261101_artists.xml.gz").matches(500_750_836));
    assert!(size("discogs_20261101_labels.xml.gz").matches(91_699_369));
    assert!(size("discogs_20261101_releases.xml.gz").matches(11_361_019_345));
    // A download cut short is too small for its listing.
    assert!(!size("discogs_20261101_artists.xml.gz").matches(450_000_000));
    assert!(!size("discogs_20261101_releases.xml.gz").matches(11_300_000_000));
}

#[test]
fn a_page_without_a_table_lists_nothing() {
    assert_eq!(listing::parse("<html><body>Not found</body></html>"), []);
}
