//! Each command, driven through the binary against the stub Discogs API.

#[path = "../../sources/tests/support/stub.rs"]
mod stub;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::Duration;

use stub::{Stub, reply};

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

/// A config in a fresh directory named for the test, pointing at `stub`.
fn setup(test: &str, stub: &Stub) -> Setup {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("commands")
        .join(test);
    let _ = fs::remove_dir_all(&dir);
    let data_dir = dir.join("data");
    fs::create_dir_all(&data_dir).unwrap();
    let config = dir.join("config.toml");
    fs::write(
        &config,
        format!(
            "discogs_token = \"made-up-token\"\ndata_dir = \"{}\"\ndiscogs_api_url = \"{}\"\n",
            data_dir.display(),
            stub.url
        ),
    )
    .unwrap();
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
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

fn requested(stub: &Stub) -> Vec<u64> {
    stub.received()
        .iter()
        .map(stub::Request::release_id)
        .collect()
}

#[test]
fn profile_load_sync_pause_resume_status_and_serve() {
    // Release 104's request never gets an answer: by the time serve sends it, the reply
    // to 103 is committed, and serve holds the lock while it waits.
    let (arrived_104, wait_for_104) = mpsc::channel();
    let (_never, unanswered) = mpsc::channel::<()>();
    let unanswered = Mutex::new(unanswered);
    let stub = Stub::start(move |request| {
        if request.release_id() == 104 {
            arrived_104.send(()).unwrap();
            let _ = unanswered.lock().unwrap().recv();
        }
        reply(200, &[], stub::RELEASE_JSON)
    });
    let setup = setup("all", &stub);
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

    let out = setup.ok(&["sync", "--limit", "1"]);
    assert!(
        out.contains("release 101: rating 4.21 (89), have 1234, want 567"),
        "{out}"
    );
    assert!(out.contains("requests sent: 1; reached the limit"), "{out}");
    assert_eq!(requested(&stub), [101]);

    setup.ok(&["sync", "pause"]);
    let out = setup.ok(&["sync"]);
    assert!(out.contains("requests sent: 0; paused"), "{out}");
    let status = setup.ok(&["sync", "status"]);
    assert!(status.contains("paused: yes\n"), "{status}");
    assert!(status.contains("queued: 3\n"), "{status}");
    assert!(status.contains("with values: 1\n"), "{status}");
    assert_eq!(requested(&stub), [0_u64; 0]);

    setup.ok(&["sync", "resume"]);
    let mut serve = setup
        .command(&["serve"])
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    wait_for_104.recv_timeout(Duration::from_secs(30)).unwrap();
    // A terminal sync refuses while serve holds the lock.
    let error = setup.fails(&["sync"]);
    assert!(error.contains("another sync is running"), "{error}");
    serve.kill().unwrap();
    serve.wait().unwrap();
    assert_eq!(requested(&stub), [102, 103]);

    let status = setup.ok(&["sync", "status"]);
    assert!(status.contains("paused: no\n"), "{status}");
    assert!(status.contains("queued: 1\n"), "{status}");
    assert!(status.contains("with values: 3\n"), "{status}");
    assert!(status.contains("gone: 0\n"), "{status}");
    assert!(status.contains("due for refresh: 0\n"), "{status}");
}

#[test]
fn a_refused_token_fails_naming_the_config_file() {
    let stub = Stub::start(|_| reply(403, &[], "{}"));
    let setup = setup("refused", &stub);
    let profile = setup.dir.join("profile.json");
    fs::write(&profile, PROFILE).unwrap();
    setup.ok(&["profile", "load", profile.to_str().unwrap()]);
    let error = setup.fails(&["sync"]);
    assert!(error.contains("refused the token (HTTP 403)"), "{error}");
    assert!(error.contains(setup.config.to_str().unwrap()), "{error}");
    assert!(!error.contains("made-up-token"), "{error}");
    let status = setup.ok(&["sync", "status"]);
    assert!(
        status.contains("ago: Discogs refused the token"),
        "{status}"
    );
}

#[test]
fn prints_the_data_directory_and_refuses_bad_arguments() {
    let stub = Stub::releases();
    let setup = setup("arguments", &stub);
    assert_eq!(
        setup.ok(&["data-dir"]).trim(),
        setup.dir.join("data").to_str().unwrap()
    );
    let error = setup.fails(&["sync", "--limit", "many"]);
    assert!(error.contains("--limit needs a whole number"), "{error}");
    let error = setup.fails(&["sink"]);
    assert!(error.contains("unknown command `sink`"), "{error}");
    let error = setup.fails(&["profile", "load", "/nonexistent/profile.json"]);
    assert!(error.contains("/nonexistent/profile.json"), "{error}");
    assert_eq!(requested(&stub), [0_u64; 0]);
}
