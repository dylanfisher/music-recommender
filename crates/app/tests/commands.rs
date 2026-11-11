//! Each command that doesn't reach the network, driven through the binary, and `serve`'s
//! downloads against a stub slskd. The sync itself is tested in the features crate against a
//! stub Discogs API: the binary only ever talks to the real one.

#[path = "../../sources/tests/support/stub.rs"]
mod stub;
#[path = "../../config/tests/support/setup.rs"]
mod support;

use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
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
    let mut lines = out.lines();
    assert_eq!(
        lines.next(),
        Some("4 albums, 4 with a Discogs release id; queued 4 new")
    );
    // Without a catalog the command says there is nothing to score, and succeeds.
    assert!(
        lines
            .next()
            .is_some_and(|line| line.contains("no catalog yet")),
        "{out}"
    );
    let data_dir = config::load(&setup.config).unwrap().data_dir;
    let kept = features::profile::kept(&data_dir).unwrap().unwrap();
    assert_eq!(kept.albums.len(), 4);
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

/// Adds `api_port = port` to the setup's config.
fn with_port(setup: &Setup, port: u16) {
    let text = fs::read_to_string(&setup.config).unwrap();
    fs::write(&setup.config, format!("api_port = {port}\n{text}")).unwrap();
}

/// Starts `serve` on any free port, which it binds itself and prints, and returns it with
/// that port, read from its first line of output. Picking a free port here and handing it
/// over would leave a moment in which a test running beside this one could take it.
fn serve(setup: &Setup) -> (Child, u16) {
    let (child, port, _) = serve_lines(setup);
    (child, port)
}

/// `serve`, and the lines it prints after its port, as it prints them.
fn serve_lines(setup: &Setup) -> (Child, u16, mpsc::Receiver<String>) {
    with_port(setup, 0);
    let mut child = setup
        .command(&["serve"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let (sender, first) = mpsc::channel();
    let (rest, lines) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let read = out.read_line(&mut line).map(|_| line);
        let _ = sender.send(read);
        // Reads the rest until serve ends: with the pipe closed, its next line would fail.
        // A caller that dropped the lines doesn't stop the reading.
        for line in out.lines() {
            let _ = rest.send(line?);
        }
        io::Result::Ok(())
    });
    // A bound on a serve that hangs before printing, not a wait the test relies on.
    let line = first.recv_timeout(Duration::from_secs(30));
    let port = line.as_ref().ok().and_then(|read| {
        read.as_ref()
            .ok()?
            .trim_end()
            .strip_prefix("the API is listening on port ")?
            .parse()
            .ok()
    });
    let Some(port) = port else {
        // It may have exited already, which `wait` then reports.
        let _ = child.kill();
        let status = child.wait();
        panic!("serve didn't say its port ({status:?}) within 30 s: {line:?}");
    };
    (child, port, lines)
}

/// The next line from `lines` that contains `text`, killing `child` if none comes.
fn line_with(lines: &mpsc::Receiver<String>, child: &mut Child, text: &str) -> String {
    let mut seen = Vec::new();
    // A bound on a serve that hangs, not a wait the test relies on.
    while let Ok(line) = lines.recv_timeout(Duration::from_secs(30)) {
        if line.contains(text) {
            return line;
        }
        seen.push(line);
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("serve printed no line with {text:?} within 30 s of the last: {seen:?}");
}

/// Sends a request with the player token to `port` and returns the whole reply.
fn post(port: u16, path: &str, body: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let request = format!(
        "POST {path} HTTP/1.1\r\nAuthorization: Bearer {}\r\nContent-Length: {}\r\n\r\n{body}",
        support::PLAYER_TOKEN,
        body.len()
    );
    stream.write_all(request.as_bytes()).unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).unwrap();
    reply
}

#[test]
fn serve_answers_the_api() {
    // The queue is empty, so the sync half of serve sends nothing and only waits.
    let setup = setup("serve");
    let (mut child, port) = serve(&setup);
    let reply = post(port, "/v1/ping", "{}");
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
    with_downloads(&setup, &slskd.url);
    let snapshot = setup.dir.join("snapshot.json");
    fs::write(&snapshot, SNAPSHOT).unwrap();
    setup.ok(&["downloads", "add", snapshot.to_str().unwrap()]);
    let (mut child, _) = serve(&setup);
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
            "catalog built from the set of 20000101: 10 artists, 5 labels, 2 masters, 12 releases\n"
        ),
        "{out}"
    );
    assert!(out.contains("releases: 12 records, all of "), "{out}");
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

/// A profile of one album, tagged with the fixture set's release 2001, played `plays` times.
fn seed_profile(plays: u64) -> String {
    format!(
        r#"{{"albums": [{{"artist": "Someone Else", "title": "Album 2001", "track_count": 1,
           "durations": [60], "catalog_numbers": [], "barcodes": [],
           "discogs_release_id": 2001, "plays": {plays}, "loved": 0}}]}}"#
    )
}

#[test]
fn profile_load_scores_the_library_and_again_only_when_the_seeds_change() {
    let setup = setup("profile-scores");
    with_dumps(&setup);
    setup.ok(&["dumps", "import"]);
    let profile = setup.dir.join("profile.json");
    fs::write(&profile, seed_profile(3)).unwrap();
    let file = profile.to_str().unwrap();
    let out = setup.ok(&["profile", "load", file]);
    assert!(
        out.contains("scores: computed from the set of 20000101 and the kept profile: "),
        "{out}"
    );
    let out = setup.ok(&["profile", "load", file]);
    assert!(
        out.contains("scores: already from the set of 20000101"),
        "{out}"
    );
    fs::write(&profile, seed_profile(4)).unwrap();
    let out = setup.ok(&["profile", "load", file]);
    assert!(out.contains("scores: computed from"), "{out}");
    let status = setup.ok(&["sync", "status"]);
    assert!(
        status.contains("\nscores: the set of 20000101 and 1 seeds, computed "),
        "{status}"
    );
    assert!(status.contains("\nscores rebuilding: no"), "{status}");
}

#[test]
fn serve_scores_at_its_start_and_after_a_profile_is_posted() {
    let setup = setup("serve-scores");
    with_dumps(&setup);
    setup.ok(&["dumps", "import"]);
    let (mut child, port, lines) = serve_lines(&setup);
    // At its start there is no profile, which stops nothing.
    line_with(&lines, &mut child, "scores: no profile kept yet");
    let reply = post(port, "/v1/profile", &seed_profile(3));
    assert!(reply.starts_with("HTTP/1.1 200 "), "{reply}");
    let line = line_with(&lines, &mut child, "scores: ");
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(
        line.starts_with("scores: computed from the set of 20000101 and the kept profile: "),
        "{line}"
    );
    let data_dir = config::load(&setup.config).unwrap().data_dir;
    let list = features::rescore::top(&data_dir, 10).unwrap().unwrap();
    assert_eq!(list.computed.seeds, 1);
    assert!(!list.candidates.is_empty());
}

#[test]
fn dumps_import_and_fetch_refuse_a_config_without_the_table() {
    let setup = setup("dumps-no-table");
    let err = setup.fails(&["dumps", "import"]);
    assert!(err.contains("no [dumps] table"), "{err}");
    // The fetch refuses before it sends any request.
    let err = setup.fails(&["dumps", "fetch"]);
    assert!(err.contains("no [dumps] table"), "{err}");
    let err = setup.fails(&["dumps", "prune"]);
    assert!(err.contains("unknown command `dumps prune`"), "{err}");
}

/// A profile of Cora Seed's 2001 and Ann Example's 1003 in the fixture set.
const SEEDS_PROFILE: &str = r#"{"albums": [
  {"artist": "Cora Seed", "title": "Seed Album", "track_count": 1, "durations": [60],
   "catalog_numbers": [], "barcodes": [], "discogs_release_id": 2001, "plays": 3, "loved": 0},
  {"artist": "Ann Example", "title": "Untitled", "track_count": 1, "durations": [60],
   "catalog_numbers": [], "barcodes": [], "discogs_release_id": 1003, "plays": 1, "loved": 0}
]}"#;

/// A setup with the fixture set imported, and `SEEDS_PROFILE` loaded when `profile`.
fn recommend_setup(test: &str, profile: bool) -> Setup {
    let setup = setup(test);
    with_dumps(&setup);
    setup.ok(&["dumps", "import"]);
    if profile {
        let file = setup.dir.join("profile.json");
        fs::write(&file, SEEDS_PROFILE).unwrap();
        setup.ok(&["profile", "load", file.to_str().unwrap()]);
    }
    setup
}

#[test]
fn recommend_prints_the_whole_library_a_seed_s_and_the_missing_albums() {
    let setup = recommend_setup("recommend", true);
    let out = setup.ok(&["recommend"]);
    assert!(
        out.starts_with(
            "the whole library: the list computed from the set of 20000101 and 2 seeds"
        ),
        "{out}"
    );
    assert!(out.contains("\nalbums\n"), "{out}");
    assert!(
        out.contains("  Gus Other – Produced Album (2001), release 2003\n"),
        "{out}"
    );
    assert!(
        out.contains("Producer: Dee Producer, Producer on 1 album you have"),
        "{out}"
    );
    assert!(out.contains("  DJ Ann – DJ Album, release 2008\n"), "{out}");
    assert!(out.contains("\nartists\n"), "{out}");
    assert!(
        out.contains("  Gus Other, artist 23, from 3 albums\n"),
        "{out}"
    );

    let out = setup.ok(&["recommend", "--album", "2001"]);
    assert!(
        out.starts_with("the seed album 2001: Cora Seed – Seed Album\n"),
        "{out}"
    );
    assert!(out.contains("release 2003\n"), "{out}");
    assert!(!out.contains("release 2008\n"), "{out}");

    let out = setup.ok(&["recommend", "--artist", "cora seed"]);
    assert!(out.starts_with("the 1 seed album by cora seed\n"), "{out}");
    assert!(!out.contains("release 2008\n"), "{out}");

    let out = setup.ok(&["recommend", "--missing"]);
    assert!(
        out.contains("only the missing albums of its artists"),
        "{out}"
    );
    assert!(
        out.contains("  Cora Seed – Second Album, release 2002\n"),
        "{out}"
    );
    assert!(!out.contains("release 2003\n"), "{out}");
    assert!(!out.contains("\nartists\n"), "{out}");
    let out = setup.ok(&[
        "recommend",
        "--artist",
        "Cora Seed",
        "--missing",
        "--limit",
        "1",
    ]);
    assert!(out.contains("  1. "), "{out}");
    assert!(!out.contains("  2. "), "{out}");

    // One album and one artist.
    let out = setup.ok(&["recommend", "--limit", "1"]);
    assert_eq!(out.matches("  1. ").count(), 2, "{out}");
    assert!(!out.contains("  2. "), "{out}");
}

#[test]
fn recommend_refuses_bad_arguments_and_what_has_no_seed() {
    let setup = recommend_setup("recommend-refused", false);
    let err = setup.fails(&["recommend"]);
    assert!(err.contains("no profile kept yet"), "{err}");
    let file = setup.dir.join("profile.json");
    fs::write(&file, SEEDS_PROFILE).unwrap();
    setup.ok(&["profile", "load", file.to_str().unwrap()]);
    let err = setup.fails(&["recommend", "--album", "9999"]);
    assert!(err.contains("tagged with release 9999"), "{err}");
    let err = setup.fails(&["recommend", "--artist", "Nobody"]);
    assert!(err.contains("`Nobody`"), "{err}");
    let err = setup.fails(&["recommend", "--album", "two"]);
    assert!(
        err.contains("--album needs a release id, not `two`"),
        "{err}"
    );
    let err = setup.fails(&["recommend", "--limit", "many"]);
    assert!(err.contains("--limit needs a whole number"), "{err}");
    for args in [
        &["recommend", "--album", "2001", "--artist", "Cora Seed"][..],
        &["recommend", "--album"],
        &["recommend", "--fast"],
        &["recommend", "--missing", "--missing"],
    ] {
        let err = setup.fails(args);
        assert!(err.contains("unknown command"), "{err}");
    }
}

#[test]
fn evaluate_reports_a_split_the_same_on_a_rerun_and_refuses_bad_arguments() {
    let setup = recommend_setup("evaluate", false);
    let err = setup.fails(&["evaluate"]);
    assert!(err.contains("no profile kept yet"), "{err}");
    let file = setup.dir.join("profile.json");
    fs::write(&file, SEEDS_PROFILE).unwrap();
    setup.ok(&["profile", "load", file.to_str().unwrap()]);
    let out = setup.ok(&["evaluate"]);
    assert!(
        out.starts_with(
            "the split of seed 1: 1 of the tagged albums' 2 candidates hidden, 1 album\n"
        ),
        "{out}"
    );
    assert!(out.contains("\nscored from 1 kept seeds: "), "{out}");
    assert!(out.contains("\nfound: "), "{out}");
    assert!(out.contains("\ntook "), "{out}");
    // The same again, but for the time, which the real clock reads.
    let without_time = |out: &str| {
        out.lines()
            .filter(|line| !line.starts_with("took "))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(without_time(&setup.ok(&["evaluate"])), without_time(&out));
    let out = setup.ok(&["evaluate", "--seed", "7"]);
    assert!(out.starts_with("the split of seed 7: "), "{out}");
    let err = setup.fails(&["evaluate", "--seed", "seven"]);
    assert!(
        err.contains("--seed needs a whole number, not `seven`"),
        "{err}"
    );
    for args in [&["evaluate", "--fast"][..], &["evaluate", "--seed"]] {
        let err = setup.fails(args);
        assert!(err.contains("unknown command"), "{err}");
    }
}
