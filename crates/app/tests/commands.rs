//! Each command that doesn't reach the network, driven through the binary. The sync itself
//! is tested in the features crate against a stub Discogs API: the binary only ever talks to
//! the real one.

#[path = "../../config/tests/support/setup.rs"]
mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};

const PROFILE: &str = r#"{"albums": [
  {"artist": "A", "title": "One", "track_count": 1, "durations": [60], "catalog_numbers": [],
   "barcodes": [], "discogs_release_id": 101, "plays": 1, "loved": 0},
  {"artist": "A", "title": "Two", "track_count": 1, "durations": [60], "catalog_numbers": [],
   "barcodes": [], "discogs_release_id": 102, "plays": 1, "loved": 0},
  {"artist": "B", "title": "Three", "track_count": 1, "durations": [60], "catalog_numbers": [],
   "barcodes": [], "discogs_release_id": 103, "plays": 1, "loved": 0},
  {"artist": "B", "title": "Four", "track_count": 1, "durations": [60], "catalog_numbers": [],
   "barcodes": [], "discogs_release_id": 104, "plays": 1, "loved": 0}
]}"#;

struct Setup {
    dir: PathBuf,
    config: PathBuf,
}

/// A config in a fresh directory named for the test.
fn setup(test: &str) -> Setup {
    let dir = support::fresh_dir("commands", test);
    let (config, _) = support::write_config(&dir);
    Setup { dir, config }
}

impl Setup {
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_music-recommender"));
        command.arg("--config").arg(&self.config).args(args);
        command
    }

    /// Runs a command to the end and returns its stdout, failing the test if it fails.
    fn ok(&self, args: &[&str]) -> String {
        let output = self.command(args).output().unwrap();
        assert!(output.status.success(), "{args:?}: {}", text(&output));
        String::from_utf8(output.stdout).unwrap()
    }

    /// Runs a command that should fail and returns its stderr.
    fn fails(&self, args: &[&str]) -> String {
        let output = self.command(args).output().unwrap();
        assert!(!output.status.success(), "{args:?}: {}", text(&output));
        String::from_utf8(output.stderr).unwrap()
    }
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn profile_load_status_pause_and_resume() {
    let setup = setup("all");
    let profile = setup.dir.join("profile.json");
    fs::write(&profile, PROFILE).unwrap();

    let out = setup.ok(&["profile", "load", profile.to_str().unwrap()]);
    assert_eq!(
        out.trim(),
        "4 albums, 4 with a Discogs release id; queued 4 new"
    );
    let status = setup.ok(&["sync", "status"]);
    assert!(status.contains("queued: 4\n"), "{status}");
    assert!(status.contains("with values: 0\n"), "{status}");
    assert!(status.contains("last error: none\n"), "{status}");
    assert!(
        status.contains("time left: at least 4 s at 60 a minute"),
        "{status}"
    );

    let out = setup.ok(&["sync", "pause"]);
    assert!(out.starts_with("paused; "), "{out}");
    let status = setup.ok(&["sync", "status"]);
    assert!(status.contains("paused: yes\n"), "{status}");
    // A paused sync sends nothing, so this reaches no network.
    let out = setup.ok(&["sync"]);
    assert!(out.contains("requests sent: 0; paused"), "{out}");

    assert_eq!(setup.ok(&["sync", "resume"]).trim(), "resumed");
    let status = setup.ok(&["sync", "status"]);
    assert!(status.contains("paused: no\n"), "{status}");
    assert!(status.contains("queued: 4\n"), "{status}");
}

#[test]
fn refuses_bad_arguments_and_a_bad_config() {
    let setup = setup("arguments");
    let error = setup.fails(&["sync", "--limit", "many"]);
    assert!(error.contains("--limit needs a whole number"), "{error}");
    for args in [&["sink"][..], &["sync", "--limit"], &["sync", "--fast"]] {
        let error = setup.fails(args);
        assert!(error.contains("unknown command"), "{error}");
    }
    let error = setup.fails(&["profile", "load", "/nonexistent/profile.json"]);
    assert!(error.contains("/nonexistent/profile.json"), "{error}");

    // A config others can read is refused, naming the file and not the token.
    fs::set_permissions(&setup.config, fs::Permissions::from_mode(0o644)).unwrap();
    let error = setup.fails(&["sync", "status"]);
    assert!(error.contains(setup.config.to_str().unwrap()), "{error}");
    assert!(!error.contains(support::TOKEN), "{error}");
}
