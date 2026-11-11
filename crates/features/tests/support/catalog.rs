//! A catalog imported for the features tests that read one, which include this file with
//! `#[path]` beside `clock.rs` and the config crate's `setup.rs` as `clock` and `support`:
//! the sources crate's fixture set, or a set a test writes, imported in a fresh directory.

#![allow(
    dead_code,
    reason = "each test crate that includes this file uses part of it"
)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use catalog::discogs::{Catalog, Reading};
use features::import;

use crate::clock::FakeClock;
use crate::support;

/// The catalog imported from the set `write` puts in an empty dumps folder, in a fresh
/// directory for `test` under `group`.
pub(crate) fn imported(group: &str, test: &str, write: impl FnOnce(&Path)) -> Catalog {
    let config = imported_config(group, test, write);
    let Reading::Open(catalog) = Catalog::open_read_only(&config.data_dir).unwrap() else {
        panic!("no catalog");
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
