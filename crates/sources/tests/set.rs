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

/// Discogs's own CHECKSUM of 2026-11-01, as data.discogs.com serves it: one space between
/// each hash and its name, and the dumps in no particular order.
const DISCOGS_CHECKSUM: &str = include_str!("fixtures/discogs_20261101_CHECKSUM.txt");

#[test]
fn reads_discogss_own_checksum_file() {
    let dir = folder("discogs-format");
    fs::write(dir.join("discogs_20261101_CHECKSUM.txt"), DISCOGS_CHECKSUM).unwrap();
    for dump in Dump::ALL {
        fs::write(dir.join(set::file_name("20261101", dump)), b"").unwrap();
    }
    let found = set::newest_complete(&dir).unwrap().unwrap();
    assert_eq!(
        found.sha256(Dump::Releases),
        "1d9781c775cf6ae01c9b18e1539a19cbec25dd440c99ba797d68dee8d3f6d01f"
    );
}

#[test]
fn pruning_a_folder_keeps_two_complete_sets_and_deletes_the_older_ones() {
    let dir = folder("prune");
    for date in ["20260901", "20261001", "20261101"] {
        write_set(&dir, date, &format!("discogs_{date}_CHECKSUM.txt"), &[]);
    }
    fs::write(dir.join(".discogs_20260801_releases.part"), b"").unwrap();
    fs::write(dir.join("notes.txt"), b"the owner's").unwrap();
    let mut deleted = set::prune(&dir).unwrap();
    deleted.sort();
    let mut expected: Vec<String> = Dump::ALL
        .iter()
        .map(|&dump| set::file_name("20260901", dump))
        .collect();
    expected.push("discogs_20260901_CHECKSUM.txt".to_string());
    expected.push(".discogs_20260801_releases.part".to_string());
    expected.sort();
    assert_eq!(deleted, expected);
    assert!(dir.join("notes.txt").exists());
    assert_eq!(
        set::newest_complete(&dir).unwrap().unwrap().date,
        "20261101"
    );
    assert!(dir.join("discogs_20261001_CHECKSUM.txt").exists());
}

#[test]
fn reads_a_checksum_files_lines() {
    let listed = set::checksums(DISCOGS_CHECKSUM).unwrap();
    assert_eq!(listed.len(), 4);
    assert_eq!(listed[0].0, "discogs_20261101_labels.xml.gz");
}
