//! Each command that doesn't reach the network, driven through the binary, and `serve`'s
//! downloads against a stub slskd. The sync itself is tested in the features crate against a
//! stub Discogs API: the binary only ever talks to the real one.

#[path = "../../sources/tests/support/stub.rs"]
mod stub;
#[path = "../../config/tests/support/setup.rs"]
mod support;

use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

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
    assert!(
        status.contains("library: 4 queued, 0 with values, 0 failed, 0 gone, 0 due"),
        "{status}"
    );
    assert!(status.contains("running: no\n"), "{status}");
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
    assert!(status.contains("library: 4 queued"), "{status}");
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

const SNAPSHOT: &str = r#"{"release_id": 4242, "artist": "Made-Up Artist", "title": "Made-Up Album",
  "year": 1999, "tracklists": [[{"position": "1", "title": "First Song", "seconds": 245}]]}"#;

/// Adds the downloads tables to the setup's config, slskd at `slskd_url`.
fn with_downloads(setup: &Setup, slskd_url: &str) {
    let downloads = setup.dir.join("slskd-downloads");
    let staging = setup.dir.join("staging");
    fs::create_dir_all(&downloads).unwrap();
    fs::create_dir_all(&staging).unwrap();
    let text = fs::read_to_string(&setup.config).unwrap();
    fs::write(
        &setup.config,
        format!(
            "{text}\n[slskd]\nurl = \"{slskd_url}\"\napi_key = \"made-up-slskd-key\"\n\
             downloads_dir = \"{}\"\n[staging]\ndir = \"{}\"\n",
            downloads.display(),
            staging.display()
        ),
    )
    .unwrap();
}

#[test]
fn downloads_add_list_status_retry_requeue_and_cancel() {
    let setup = setup("downloads");
    with_downloads(&setup, "http://127.0.0.1:9");
    let snapshot = setup.dir.join("snapshot.json");
    fs::write(&snapshot, SNAPSHOT).unwrap();
    let file = snapshot.to_str().unwrap();

    let out = setup.ok(&["downloads", "add", file, "--format", "lossless"]);
    assert_eq!(
        out.trim(),
        "job 1: Made-Up Artist - Made-Up Album (1999), queued, lossless"
    );
    let error = setup.fails(&["downloads", "add", file]);
    assert!(
        error.contains("job 1 for this release isn't done"),
        "{error}"
    );

    let list = setup.ok(&["downloads", "list"]);
    assert_eq!(
        list.trim(),
        "1  queued  lossless  Made-Up Artist - Made-Up Album (1999)"
    );
    let status = setup.ok(&["downloads", "status", "1"]);
    assert!(status.contains(r#""state": "queued""#), "{status}");
    assert!(status.contains(r#""release_id": 4242"#), "{status}");

    let error = setup.fails(&["downloads", "retry", "1"]);
    assert!(error.contains("a queued job can't take that"), "{error}");
    let error = setup.fails(&["downloads", "requeue", "1"]);
    assert!(error.contains("a queued job can't take that"), "{error}");
    let out = setup.ok(&["downloads", "cancel", "1"]);
    assert_eq!(
        out.trim(),
        "job 1: Made-Up Artist - Made-Up Album (1999), cancelled, lossless"
    );
    let error = setup.fails(&["downloads", "status", "2"]);
    assert!(error.contains("no job has that id"), "{error}");
}

#[test]
fn downloads_refuse_a_bad_snapshot_a_bad_format_and_a_config_without_the_tables() {
    let setup = setup("downloads-bad");
    let snapshot = setup.dir.join("snapshot.json");
    fs::write(&snapshot, SNAPSHOT).unwrap();
    let file = snapshot.to_str().unwrap();
    let error = setup.fails(&["downloads", "list"]);
    assert!(error.contains("no [slskd] and [staging] tables"), "{error}");

    with_downloads(&setup, "http://127.0.0.1:9");
    let error = setup.fails(&["downloads", "add", file, "--format", "wav"]);
    assert!(error.contains("--format"), "{error}");
    fs::write(&snapshot, SNAPSHOT.replace("\"1\"", "\"\"")).unwrap();
    let error = setup.fails(&["downloads", "add", file]);
    assert!(
        error.contains("snapshot.tracklists[0][0].position is empty"),
        "{error}"
    );
    for args in [&["downloads", "retry", "x"][..], &["downloads", "cancel"]] {
        let error = setup.fails(args);
        assert!(
            error.contains("unknown command") || error.contains("id"),
            "{error}"
        );
    }
}

#[test]
fn serve_searches_slskd_for_a_job_added_from_the_command_line() {
    // Every search started and read back is complete, with no peers.
    let slskd = Stub::start(|request| match request.method.as_str() {
        "POST" | "GET" => reply(
            200,
            &[],
            r#"{"id": "s1", "isComplete": true, "responses": []}"#,
        ),
        _ => reply(204, &[], ""),
    });
    let setup = setup("serve-downloads");
    with_port(&setup, free_port());
    with_downloads(&setup, &slskd.url);
    let snapshot = setup.dir.join("snapshot.json");
    fs::write(&snapshot, SNAPSHOT).unwrap();
    setup.ok(&["downloads", "add", snapshot.to_str().unwrap()]);
    let mut child = setup.command(&["serve"]).spawn().unwrap();
    let search = slskd.next_request();
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(
        (search.method.as_str(), search.path.as_str()),
        ("POST", "/api/v0/searches")
    );
    assert_eq!(search.header("x-api-key"), Some("made-up-slskd-key"));
    let body = String::from_utf8(search.body).unwrap();
    assert!(body.contains("made up artist made up album"), "{body}");
}

/// Adds a `[dumps]` table to the setup's config, pointing at a copy of the sources crate's
/// fixture set.
fn with_dumps(setup: &Setup) {
    let folder = setup.dir.join("discogs");
    fs::create_dir_all(&folder).unwrap();
    let fixtures =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../sources/tests/fixtures/dumps");
    for entry in fs::read_dir(fixtures).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), folder.join(entry.file_name())).unwrap();
    }
    let config_text = fs::read_to_string(&setup.config).unwrap()
        + &format!("[dumps]\ndir = \"{}\"\n", folder.display());
    fs::write(&setup.config, config_text).unwrap();
}

#[test]
fn dumps_import_builds_the_catalog_and_sync_status_shows_it() {
    let setup = setup("dumps-import");
    with_dumps(&setup);
    let out = setup.ok(&["dumps", "import"]);
    assert!(
        out.ends_with(
            "catalog built from the set of 20000101: 5 artists, 3 labels, 2 masters, 3 releases\n"
        ),
        "{out}"
    );
    assert!(out.contains("releases: 3 records, all of "), "{out}");
    let status = setup.ok(&["sync", "status"]);
    assert!(
        status.contains("\ncatalog: the set of 20000101, swapped in "),
        "{status}"
    );
    assert_eq!(
        setup.ok(&["dumps", "import"]).trim(),
        "the catalog is already built from the set of 20000101"
    );
}

#[test]
fn dumps_import_refuses_a_config_without_the_table() {
    let setup = setup("dumps-no-table");
    let err = setup.fails(&["dumps", "import"]);
    assert!(err.contains("no [dumps] table"), "{err}");
    let err = setup.fails(&["dumps", "fetch"]);
    assert!(err.contains("unknown command `dumps fetch`"), "{err}");
}
