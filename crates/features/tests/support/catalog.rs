//! A catalog imported for the features tests that read one, which include this file with
//! `#[path]` beside `clock.rs` and the config crate's `setup.rs` as `clock` and `support`:
//! the sources crate's fixture set, or a set a test writes, imported in a fresh directory;
//! and the library albums the tests seed it with.

#![allow(
    dead_code,
    reason = "each test crate that includes this file uses part of it"
)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use catalog::discogs::{Catalog, Reading};
use catalog::score::{Computed, Scores};
use features::import;
use features::profile::Album;
use features::rescore::{self, Listed};

use crate::clock::FakeClock;
use crate::support;

/// The catalog imported from the set `write` puts in an empty dumps folder, in a fresh
/// directory for `test` under `group`.
pub(crate) fn imported(group: &str, test: &str, write: impl FnOnce(&Path)) -> Catalog {
    open(&imported_config(group, test, write).data_dir)
}

/// The catalog in `data_dir`, which an import built.
pub(crate) fn open(data_dir: &Path) -> Catalog {
    let Reading::Open(catalog) = Catalog::open_read_only(data_dir).unwrap() else {
        panic!("no catalog in {}", data_dir.display());
    };
    catalog
}

/// `imported`'s config, with a `[dumps]` table for the set, for a test that reads the
/// catalog through the data directory.
pub(crate) fn imported_config(
    group: &str,
    test: &str,
    write: impl FnOnce(&Path),
) -> config::Config {
    let dir = support::fresh_dir(group, test);
    let (path, _) = support::write_config(&dir);
    let dumps = dir.join("discogs");
    fs::create_dir_all(&dumps).unwrap();
    write(&dumps);
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(format!("[dumps]\ndir = \"{}\"\n", dumps.display()).as_bytes())
        .unwrap();
    let config = config::load(&path).unwrap();
    import::run(&config, &FakeClock::new(), import::BATCH, &mut |_| {}).unwrap();
    config
}

/// Copies the sources crate's fixture set into `dumps`.
pub(crate) fn fixture_set(dumps: &Path) {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../sources/tests/fixtures");
    for entry in fs::read_dir(fixtures.join("dumps")).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), dumps.join(entry.file_name())).unwrap();
    }
}

/// A library album of one track by `artist`, titled `title`, tagged with `release_id`,
/// played `plays` times.
pub(crate) fn album(artist: &str, title: &str, release_id: Option<u64>, plays: u64) -> Album {
    Album {
        artist: artist.to_string(),
        title: title.to_string(),
        track_count: 1,
        durations: vec![180],
        catalog_numbers: Vec::new(),
        barcodes: Vec::new(),
        discogs_release_id: release_id,
        plays,
        loved: 0,
        rating: None,
    }
}

/// A library album by no artist of the catalog, tagged with `release_id`, played `plays`
/// times.
pub(crate) fn tagged(release_id: u64, plays: u64) -> Album {
    album(
        "Someone Else",
        &format!("Album {release_id}"),
        Some(release_id),
        plays,
    )
}

/// A list stored in the data directory, read back.
#[derive(Debug)]
pub(crate) struct Stored {
    /// What it was computed from and when.
    pub(crate) computed: Computed,
    /// Its candidates, best first.
    pub(crate) candidates: Vec<Listed>,
}

/// The first `limit` candidates of the list stored in `data_dir`; `None` when there is no
/// list, or one of another shape.
pub(crate) fn stored(data_dir: &Path, limit: u64) -> Option<Stored> {
    let Reading::Open(scores) = Scores::open_read_only(data_dir).unwrap() else {
        return None;
    };
    let computed = scores.computed().unwrap();
    let ranks: Vec<u64> = (1..=computed.candidates.min(limit)).collect();
    let candidates = scores
        .rows(&ranks)
        .unwrap()
        .into_iter()
        .zip(ranks)
        .map(|(row, rank)| rescore::listed(rank, row).unwrap())
        .collect();
    Some(Stored {
        computed,
        candidates,
    })
}
