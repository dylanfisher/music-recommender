//! Test setup shared by the config, features and app tests, which include this file with
//! `#[path]`: a fresh directory for a test and a valid config file in it. The catalog tests
//! can't see the config crate and keep their own directory helper.

#![allow(
    dead_code,
    reason = "each test crate that includes this file uses part of it"
)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// The Discogs token the config files hold.
pub(crate) const TOKEN: &str = "made-up-token";
/// The player token the config files hold.
pub(crate) const PLAYER_TOKEN: &str = "made-up-player-token";

/// A fresh, empty directory for `test` under `group`, in the including crate's temp
/// directory.
pub(crate) fn fresh_dir(group: &str, test: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(group)
        .join(test);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Writes a valid `config.toml` (mode 0600) in `dir` with a data directory `data` created
/// inside it, and returns the config file's path and the data directory's.
pub(crate) fn write_config(dir: &Path) -> (PathBuf, PathBuf) {
    let data_dir = dir.join("data");
    fs::create_dir_all(&data_dir).unwrap();
    let path = dir.join("config.toml");
    fs::write(
        &path,
        format!(
            "discogs_token = \"{TOKEN}\"\nplayer_token = \"{PLAYER_TOKEN}\"\n\
             data_dir = \"{}\"\n",
            data_dir.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    (path, data_dir)
}
