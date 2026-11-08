//! Each command that doesn't reach the network, driven through the binary. The sync itself
//! is tested in the features crate against a stub Discogs API: the binary only ever talks to
//! the real one.

#[path = "../../config/tests/support/setup.rs"]
mod support;

use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

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

/// A port nothing listens on right now. Another process could take it before the binary
/// does; then the test fails on the bind, naming the port.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Adds `api_port = port` to the setup's config.
fn with_port(setup: &Setup, port: u16) {
    let text = fs::read_to_string(&setup.config).unwrap();
    fs::write(&setup.config, format!("api_port = {port}\n{text}")).unwrap();
}

#[test]
fn serve_answers_the_api() {
    // The queue is empty, so the sync half of serve sends nothing and only waits.
    let setup = setup("serve");
    let port = free_port();
    with_port(&setup, port);
    let mut child = setup.command(&["serve"]).spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut stream = loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(stream) => break stream,
            Err(error) => {
                if let Some(status) = child.try_wait().unwrap() {
                    panic!("serve exited with {status} before listening");
                }
                assert!(Instant::now() < deadline, "serve never listened: {error}");
                std::thread::yield_now();
            }
        }
    };
    let request = format!(
        "POST /v1/ping HTTP/1.1\r\nAuthorization: Bearer {}\r\nContent-Length: 2\r\n\r\n{{}}",
        support::PLAYER_TOKEN
    );
    stream.write_all(request.as_bytes()).unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(reply.starts_with("HTTP/1.1 200 "), "{reply}");
    assert!(reply.contains(r#""ok":true"#), "{reply}");
}

#[test]
fn serve_fails_when_its_port_is_taken() {
    let setup = setup("serve_taken");
    let taken = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
    let port = taken.local_addr().unwrap().port();
    with_port(&setup, port);
    let error = setup.fails(&["serve"]);
    assert!(error.contains(&format!("port {port}")), "{error}");
}
