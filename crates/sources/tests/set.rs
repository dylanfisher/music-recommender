//! Finding the newest complete set of dumps in a folder.

use std::fs;
use std::path::{Path, PathBuf};

use sources::discogs::set::{self, Dump};

const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// A fresh, empty folder named for the test.
fn folder(test: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("set")
        .join(test);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Writes the set of `date` into `dir`, its CHECKSUM named `checksum_name`, leaving out
/// the dumps in `missing`.
fn write_set(dir: &Path, date: &str, checksum_name: &str, missing: &[Dump]) {
    let mut checksum = String::new();
    for dump in Dump::ALL {
        let name = set::file_name(date, dump);
        checksum = checksum + SHA + "  " + &name + "\n";
        if !missing.contains(&dump) {
            fs::write(dir.join(&name), b"").unwrap();
        }
    }
    fs::write(dir.join(checksum_name), checksum).unwrap();
}

#[test]
fn the_newest_complete_set_skips_newer_ones_still_downloading() {
    let dir = folder("newest");
    write_set(&dir, "20261001", "discogs_20261001_CHECKSUM.txt", &[]);
    write_set(&dir, "20261101", "discogs_20261101_CHECKSUM.txt", &[]);
    // A newer set whose releases dump isn't there yet, and one whose CHECKSUM is still a part.
    write_set(
        &dir,
        "20261201",
        "discogs_20261201_CHECKSUM.txt",
        &[Dump::Releases],
    );
    write_set(&dir, "20270101", ".discogs_20270101_CHECKSUM.part", &[]);
    let found = set::newest_complete(&dir).unwrap().unwrap();
    assert_eq!(found.date, "20261101");
    assert_eq!(
        found.path(Dump::Masters),
        dir.join("discogs_20261101_masters.xml.gz")
    );
    assert_eq!(found.sha256(Dump::Masters), SHA);
}

#[test]
fn a_folder_without_a_complete_set_has_none() {
    let dir = folder("none");
    write_set(
        &dir,
        "20261101",
        "discogs_20261101_CHECKSUM.txt",
        &[Dump::Artists],
    );
    assert!(set::newest_complete(&dir).unwrap().is_none());
}

#[test]
fn a_missing_folder_fails_naming_it() {
    let dir = folder("missing").join("not-mounted");
    let error = set::newest_complete(&dir).unwrap_err().to_string();
    assert!(error.contains("not-mounted"), "{error}");
}
