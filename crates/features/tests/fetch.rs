//! The fetch against a stub of data.discogs.com: the newest set found in this year's and,
//! in January, last year's listing, its CHECKSUM first and each dump through a `.part`
//! checked against it, the waits the site asks for on the made-up clock, the refusals, the
//! sets it leaves for the import to delete, and the monthly loop of fetch and import.

#[path = "support/clock.rs"]
mod clock;
#[path = "../../sources/tests/support/stub.rs"]
mod stub;
#[path = "../../config/tests/support/setup.rs"]
mod support;

use std::fs;
use std::io::Write as _;
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use clock::FakeClock;
use config::Config;
use features::clock::Clock;
use features::fetch::{self, Summary};
use features::import;
use features::rescore::Requests;
use sources::discogs::download::Client;
use stub::{Request, Stub, reply, reply_bytes};

/// The set the stub serves: the sources crate's fixture set under a December date.
const DATE: &str = "20261201";
const FIXTURE_DATE: &str = "20000101";
const DUMPS: [&str; 4] = ["artists", "labels", "masters", "releases"];
const STOP: &str = "stop serving";

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../sources/tests/fixtures/dumps")
}

/// The fixture's dump `dump`.
fn dump_bytes(dump: &str) -> Vec<u8> {
    fs::read(fixtures().join(format!("discogs_{FIXTURE_DATE}_{dump}.xml.gz"))).unwrap()
}

/// The fixture's CHECKSUM as the set of `date`'s, in Discogs's one-space form.
fn checksum(date: &str) -> String {
    fs::read_to_string(fixtures().join(format!("discogs_{FIXTURE_DATE}_CHECKSUM.txt")))
        .unwrap()
        .replace(FIXTURE_DATE, date)
        .replace("  ", " ")
}

/// A year's listing page in the shape data.discogs.com sends, its sizes in bytes.
fn listing(files: &[(String, usize)]) -> String {
    let rows = files.iter().fold(String::new(), |rows, (name, size)| {
        let year = &name["discogs_".len().."discogs_".len() + 4];
        rows + &format!(
            "2026-12-01 16:40:37             {size} B          \
             <a href=\"?download=data%2F{year}%2F{name}\">{name}</a>\n"
        )
    });
    format!(
        "<html><body><h2>Discogs Data</h2>\n    <pre>Last Modified                   Size           \
         Name\n---------------------\n                                               \
         <a href=\"?prefix=data%2F\">../</a>\n{rows}</pre></body></html>\n"
    )
}

/// What the stub serves: the 2026 listing with the set of `DATE`, an empty 2027 listing,
/// and each file, unless `overrides` answers the request first.
fn site(
    overrides: impl Fn(&Request) -> Option<Vec<u8>> + Send + 'static,
    checksum_text: String,
) -> Stub {
    Stub::start_bytes(move |request| {
        if let Some(answer) = overrides(request) {
            return answer;
        }
        let path = &request.path;
        if path == "/?prefix=data%2F2027%2F" {
            return reply(200, &[], &listing(&[])).into_bytes();
        }
        if path == "/?prefix=data%2F2026%2F" {
            let mut files = vec![(format!("discogs_{DATE}_CHECKSUM.txt"), checksum_text.len())];
            for dump in DUMPS {
                files.push((
                    format!("discogs_{DATE}_{dump}.xml.gz"),
                    dump_bytes(dump).len(),
                ));
            }
            return reply(200, &[], &listing(&files)).into_bytes();
        }
        if path.ends_with("_CHECKSUM.txt") {
            return reply(200, &[], &checksum_text).into_bytes();
        }
        for dump in DUMPS {
            if path.ends_with(&format!("_{dump}.xml.gz")) {
                return reply_bytes(200, &[], &dump_bytes(dump));
            }
        }
        reply(404, &[], "").into_bytes()
    })
}

struct Setup {
    config: Config,
    dumps: PathBuf,
}

/// A config in a fresh directory named for the test, `[dumps]` pointing at an empty
/// folder, with `monthly` as given.
fn setup(test: &str, monthly: bool) -> Setup {
    let dir = support::fresh_dir("fetch", test);
    let (path, _) = support::write_config(&dir);
    let dumps = dir.join("discogs");
    fs::create_dir_all(&dumps).unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(
            format!(
                "[dumps]\ndir = \"{}\"\nmonthly = {monthly}\n",
                dumps.display()
            )
            .as_bytes(),
        )
        .unwrap();
    Setup {
        config: config::load(&path).unwrap(),
        dumps,
    }
}

impl Setup {
    fn run(
        &self,
        stub: &Stub,
        clock: &FakeClock,
    ) -> (Result<Summary, fetch::FetchError>, Vec<String>) {
        let mut lines = Vec::new();
        let result = fetch::run(&self.config, &Client::new(&stub.url), clock, &mut |line| {
            lines.push(line.to_string());
        });
        (result, lines)
    }

    fn files(&self) -> Vec<String> {
        support::files(&self.dumps)
    }

    /// Imports the newest complete set, so the catalog is from it.
    fn import(&self) {
        import::run(&self.config, &FakeClock::new(), 100, &mut |_| {}).unwrap();
    }
}

fn paths(stub: &Stub) -> Vec<String> {
    stub.received().into_iter().map(|r| r.path).collect()
}

/// The set of `DATE`'s file names, sorted.
fn set_files(date: &str) -> Vec<String> {
    let mut files: Vec<String> = DUMPS
        .iter()
        .map(|dump| format!("discogs_{date}_{dump}.xml.gz"))
        .collect();
    files.push(format!("discogs_{date}_CHECKSUM.txt"));
    files.sort();
    files
}

#[test]
fn fetches_the_newest_set_checksum_first_from_last_years_listing_in_january() {
    let setup = setup("january", false);
    let stub = site(|_| None, checksum(DATE));
    let (result, lines) = setup.run(&stub, &FakeClock::new());
    assert_eq!(
        result.unwrap().to_string(),
        format!("fetched the set of {DATE}: 5 files")
    );
    assert_eq!(
        paths(&stub),
        [
            "/?prefix=data%2F2027%2F".to_string(),
            "/?prefix=data%2F2026%2F".to_string(),
            format!("/?download=data%2F2026%2Fdiscogs_{DATE}_CHECKSUM.txt"),
            format!("/?download=data%2F2026%2Fdiscogs_{DATE}_artists.xml.gz"),
            format!("/?download=data%2F2026%2Fdiscogs_{DATE}_labels.xml.gz"),
            format!("/?download=data%2F2026%2Fdiscogs_{DATE}_masters.xml.gz"),
            format!("/?download=data%2F2026%2Fdiscogs_{DATE}_releases.xml.gz"),
        ]
    );
    assert_eq!(setup.files(), set_files(DATE));
    assert_eq!(
        fs::read(setup.dumps.join(format!("discogs_{DATE}_releases.xml.gz"))).unwrap(),
        dump_bytes("releases")
    );
    assert!(lines.iter().any(|l| l.contains("releases")), "{lines:?}");
}

#[test]
fn after_january_only_this_years_listing_is_read() {
    let setup = setup("february", false);
    let stub = site(|_| None, checksum(DATE));
    let clock = FakeClock::new();
    clock.sleep(Duration::from_hours(24 * 40));
    let (result, _) = setup.run(&stub, &clock);
    assert_eq!(
        result.unwrap().to_string(),
        "no newer set on data.discogs.com than none here"
    );
    assert_eq!(paths(&stub), ["/?prefix=data%2F2027%2F"]);
}

#[test]
fn a_set_already_here_is_not_fetched_again() {
    let setup = setup("current", false);
    let stub = site(|_| None, checksum(DATE));
    setup.run(&stub, &FakeClock::new()).0.unwrap();
    setup.import();
    stub.received();
    let (result, _) = setup.run(&stub, &FakeClock::new());
    assert_eq!(
        result.unwrap().to_string(),
        format!("no newer set on data.discogs.com than {DATE} here")
    );
    assert_eq!(paths(&stub).len(), 2);
}

#[test]
fn a_set_not_imported_yet_holds_the_next_fetch() {
    let setup = setup("pending", false);
    let stub = site(|_| None, checksum(DATE));
    setup.run(&stub, &FakeClock::new()).0.unwrap();
    stub.received();
    // No catalog yet, then a catalog that can't be read: the set is waiting either way.
    for _ in 0..2 {
        let (result, _) = setup.run(&stub, &FakeClock::new());
        assert_eq!(
            result.unwrap().to_string(),
            format!("the set of {DATE} here isn't imported yet; no fetch until it is")
        );
        assert_eq!(paths(&stub), Vec::<String>::new());
        fs::write(
            setup.config.data_dir.join("catalog.sqlite"),
            "not a database",
        )
        .unwrap();
    }
    setup.import();
    let (result, _) = setup.run(&stub, &FakeClock::new());
    assert_eq!(
        result.unwrap().to_string(),
        format!("no newer set on data.discogs.com than {DATE} here")
    );
}

#[test]
fn waits_out_retry_after_on_the_clock_and_asks_again() {
    let setup = setup("retry-after", false);
    let refused = AtomicU32::new(0);
    let stub = site(
        move |request| {
            (request.path.contains("_artists") && refused.fetch_add(1, Ordering::SeqCst) < 2)
                .then(|| reply(429, &[("Retry-After", "2750")], "").into_bytes())
        },
        checksum(DATE),
    );
    let clock = FakeClock::new();
    let (result, lines) = setup.run(&stub, &clock);
    result.unwrap();
    assert_eq!(clock.sleeps(), [Duration::from_secs(2750); 2]);
    assert!(lines.iter().any(|l| l.contains("2750 s")), "{lines:?}");
    assert_eq!(setup.files(), set_files(DATE));
}

#[test]
fn a_dump_that_doesnt_match_its_checksum_is_removed_and_the_set_left_incomplete() {
    let setup = setup("bad-checksum", false);
    let stub = site(
        |request| {
            request.path.contains("_labels").then(|| {
                let mut wrong = dump_bytes("labels");
                wrong[20] ^= 0xFF;
                reply_bytes(200, &[], &wrong)
            })
        },
        checksum(DATE),
    );
    let error = setup
        .run(&stub, &FakeClock::new())
        .0
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("labels") && error.contains("SHA-256"),
        "{error}"
    );
    // The artists dump checked and kept; no labels, no part, and the CHECKSUM still a part,
    // so the set isn't complete and the import doesn't see it.
    assert_eq!(
        setup.files(),
        [
            format!(".discogs_{DATE}_CHECKSUM.part"),
            format!("discogs_{DATE}_artists.xml.gz"),
        ]
    );
    // The next run downloads what is missing, and not the artists again.
    let stub = site(|_| None, checksum(DATE));
    setup.run(&stub, &FakeClock::new()).0.unwrap();
    assert!(!paths(&stub).iter().any(|p| p.contains("_artists")));
    assert_eq!(setup.files(), set_files(DATE));
}

#[test]
fn a_download_whose_size_isnt_the_listings_is_refused() {
    let setup = setup("short", false);
    let stub = site(
        |request| {
            request
                .path
                .contains("_masters")
                .then(|| reply_bytes(200, &[], &dump_bytes("masters")[..100]))
        },
        checksum(DATE),
    );
    let error = setup
        .run(&stub, &FakeClock::new())
        .0
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("masters") && error.contains("listing"),
        "{error}"
    );
    assert!(!setup.files().iter().any(|f| f.contains("masters")));
}

#[test]
fn a_set_whose_checksum_lacks_a_dump_is_skipped() {
    let setup = setup("three-dumps", false);
    let three: String = checksum(DATE)
        .lines()
        .filter(|line| !line.contains("releases"))
        .fold(String::new(), |text, line| text + line + "\n");
    let stub = site(|_| None, three);
    let (result, lines) = setup.run(&stub, &FakeClock::new());
    assert_eq!(
        result.unwrap().to_string(),
        "no newer set on data.discogs.com than none here"
    );
    assert!(
        lines.iter().any(|l| l.contains("doesn't list")),
        "{lines:?}"
    );
    assert_eq!(setup.files(), Vec::<String>::new());
}

#[test]
fn a_fetch_deletes_no_set() {
    let setup = setup("keeps-sets", false);
    for date in ["20261001", "20261101"] {
        for dump in DUMPS {
            fs::write(
                setup.dumps.join(format!("discogs_{date}_{dump}.xml.gz")),
                dump_bytes(dump),
            )
            .unwrap();
        }
        fs::write(
            setup.dumps.join(format!("discogs_{date}_CHECKSUM.txt")),
            checksum(date),
        )
        .unwrap();
    }
    // The import deletes the set of 20261001; the fetch then deletes nothing.
    setup.import();
    let stub = site(|_| None, checksum(DATE));
    let (result, _) = setup.run(&stub, &FakeClock::new());
    assert_eq!(
        result.unwrap().to_string(),
        format!("fetched the set of {DATE}: 5 files")
    );
    let mut kept = set_files("20261101");
    kept.extend(set_files(DATE));
    kept.sort();
    assert_eq!(setup.files(), kept);
}

#[test]
fn refuses_without_the_dumps_table() {
    let dir = support::fresh_dir("fetch", "no-table");
    let (path, _) = support::write_config(&dir);
    let config = config::load(&path).unwrap();
    let stub = site(|_| None, checksum(DATE));
    let error = fetch::run(
        &config,
        &Client::new(&stub.url),
        &FakeClock::new(),
        &mut |_| {},
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("no [dumps] table"), "{error}");
    assert_eq!(paths(&stub), Vec::<String>::new());
}

#[test]
fn the_monthly_loop_fetches_then_imports_and_checks_again_a_day_later() {
    let setup = setup("monthly", true);
    let stub = site(|_| None, checksum(DATE));
    let mut clock = FakeClock::new();
    clock.on_sleep = Box::new(|sleeps| {
        assert!(sleeps != 2, "{STOP}");
    });
    let lines = Mutex::new(Vec::new());
    let rescore = Requests::default();
    let stopped = panic::catch_unwind(AssertUnwindSafe(|| {
        fetch::serve(
            &setup.config,
            &Client::new(&stub.url),
            &clock,
            &rescore,
            &mut |line| {
                lines.lock().unwrap().push(line.to_string());
            },
        );
    }))
    .unwrap_err();
    assert_eq!(stopped.downcast_ref::<String>().unwrap(), STOP);
    assert_eq!(clock.sleeps(), [Duration::from_hours(24); 2]);
    // The catalog swapped in asks for the scores' rebuild.
    assert!(rescore.pending());
    let lines = lines.into_inner().unwrap();
    assert!(
        lines
            .iter()
            .any(|l| l.contains(&format!("catalog built from the set of {DATE}"))),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains(&format!("already built from the set of {DATE}"))),
        "{lines:?}"
    );
}

#[test]
fn the_monthly_loop_tries_again_an_hour_after_a_failure() {
    let setup = setup("monthly-failed", true);
    let stub = Stub::start(|_| reply(500, &[], ""));
    let mut clock = FakeClock::new();
    clock.on_sleep = Box::new(|sleeps| {
        assert!(sleeps != 2, "{STOP}");
    });
    let lines = Mutex::new(Vec::new());
    let rescore = Requests::default();
    let _ = panic::catch_unwind(AssertUnwindSafe(|| {
        fetch::serve(
            &setup.config,
            &Client::new(&stub.url),
            &clock,
            &rescore,
            &mut |line| {
                lines.lock().unwrap().push(line.to_string());
            },
        );
    }));
    assert_eq!(clock.sleeps(), [Duration::from_hours(1); 2]);
    // No catalog was swapped in, so no rebuild is asked for.
    assert!(!rescore.pending());
    let lines = lines.into_inner().unwrap();
    assert!(lines.iter().any(|l| l.contains("HTTP 500")), "{lines:?}");
}

#[test]
fn a_retry_after_over_an_hour_is_waited_for_an_hour() {
    let setup = setup("capped-wait", false);
    let refused = AtomicU32::new(0);
    let stub = site(
        move |request| {
            (request.path.contains("_labels") && refused.fetch_add(1, Ordering::SeqCst) < 1)
                .then(|| reply(429, &[("Retry-After", "86400")], "").into_bytes())
        },
        checksum(DATE),
    );
    let clock = FakeClock::new();
    setup.run(&stub, &clock).0.unwrap();
    assert_eq!(clock.sleeps(), [Duration::from_hours(1)]);
}

#[test]
fn the_waits_of_a_whole_run_share_one_budget() {
    let setup = setup("wait-budget", false);
    // Six waits for the 2027 listing and six for the artists dump: each under ten, twelve
    // in the run.
    let listing_refused = AtomicU32::new(0);
    let artists_refused = AtomicU32::new(0);
    let stub = site(
        move |request| {
            let refused = if request.path == "/?prefix=data%2F2027%2F" {
                &listing_refused
            } else if request.path.contains("_artists") {
                &artists_refused
            } else {
                return None;
            };
            (refused.fetch_add(1, Ordering::SeqCst) < 6)
                .then(|| reply(429, &[("Retry-After", "60")], "").into_bytes())
        },
        checksum(DATE),
    );
    let clock = FakeClock::new();
    let (result, lines) = setup.run(&stub, &clock);
    let error = result.unwrap_err().to_string();
    assert!(error.contains("10 times"), "{error}");
    assert_eq!(clock.sleeps().len(), 10);
    let waits = lines.iter().filter(|l| l.contains("waiting 60 s")).count();
    assert_eq!(waits, 10, "{lines:?}");
}

#[test]
fn in_january_a_year_not_on_the_site_yet_reads_as_empty() {
    let setup = setup("january-404", false);
    let stub = site(
        |request| {
            (request.path == "/?prefix=data%2F2027%2F")
                .then(|| reply(404, &[], "Not Found").into_bytes())
        },
        checksum(DATE),
    );
    let (result, _) = setup.run(&stub, &FakeClock::new());
    assert_eq!(
        result.unwrap().to_string(),
        format!("fetched the set of {DATE}: 5 files")
    );
}

#[test]
fn a_listing_that_fails_otherwise_fails_the_fetch() {
    let setup = setup("listing-500", false);
    let stub = site(
        |request| {
            (request.path == "/?prefix=data%2F2027%2F").then(|| reply(500, &[], "").into_bytes())
        },
        checksum(DATE),
    );
    let error = setup
        .run(&stub, &FakeClock::new())
        .0
        .unwrap_err()
        .to_string();
    assert!(error.contains("HTTP 500"), "{error}");
}

#[test]
fn a_checksum_that_cant_be_read_fails_and_leaves_no_part() {
    let setup = setup("unreadable-checksum", false);
    let stub = site(|_| None, "not a checksum file\n".to_string());
    let error = setup
        .run(&stub, &FakeClock::new())
        .0
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("CHECKSUM") && error.contains("line 1"),
        "{error}"
    );
    assert_eq!(setup.files(), Vec::<String>::new());
}

#[test]
fn a_checksum_line_beyond_the_four_dumps_is_logged_once_and_ignored() {
    let setup = setup("extra-line", false);
    let extra = format!(
        "{}0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef \
         discogs_{DATE}_readme.txt\n",
        checksum(DATE)
    );
    let stub = site(|_| None, extra.clone());
    let (result, lines) = setup.run(&stub, &FakeClock::new());
    result.unwrap();
    let mentions = lines.iter().filter(|l| l.contains("readme")).count();
    assert_eq!(mentions, 1, "{lines:?}");
    assert_eq!(setup.files(), set_files(DATE));
    setup.import();
    let stub = site(|_| None, extra);
    let (result, _) = setup.run(&stub, &FakeClock::new());
    assert_eq!(
        result.unwrap().to_string(),
        format!("no newer set on data.discogs.com than {DATE} here")
    );
}
