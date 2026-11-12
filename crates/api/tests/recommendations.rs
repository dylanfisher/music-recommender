//! `POST /v1/recommendations` over real `TcpStream`s on `127.0.0.1`, against the sources
//! crate's fixture set imported and scored as `profile load` scores it: each list a page at a
//! time in the shape docs/api.md writes, the filters, the feedback's marks left out, the
//! refusals, and the 400s.

#[path = "support/client.rs"]
mod client;
#[path = "../../features/tests/support/clock.rs"]
mod clock;
#[path = "../../config/tests/support/setup.rs"]
mod support;

use std::collections::BTreeSet;
use std::fs;
use std::io::Write as _;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use client::{Reply, post, send};
use clock::FakeClock;
use features::clock::SystemClock;
use features::list::SeedSlots;
use features::profile::{self, Album, Profile};
use features::rescore::Requests;
use features::status::{Community, Tier, Values};
use features::{import, rescore};
use serde_json::{Value, json};

const PATH: &str = "/v1/recommendations";

/// A config for `test` with the fixture set in its dumps folder, and its file's path.
fn config_file(test: &str) -> PathBuf {
    let dir = support::fresh_dir("recommendations", test);
    let (path, _) = support::write_config(&dir);
    let dumps = dir.join("discogs");
    fs::create_dir_all(&dumps).unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../sources/tests/fixtures/dumps");
    for entry in fs::read_dir(fixtures).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), dumps.join(entry.file_name())).unwrap();
    }
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(format!("[dumps]\ndir = \"{}\"\n", dumps.display()).as_bytes())
        .unwrap();
    path
}

/// A library album of one track by `artist`, titled `title`, tagged with `release_id`.
fn album(artist: &str, title: &str, release_id: u64) -> Album {
    Album {
        artist: artist.to_string(),
        title: title.to_string(),
        track_count: 1,
        durations: vec![180],
        catalog_numbers: Vec::new(),
        barcodes: Vec::new(),
        discogs_release_id: Some(release_id),
        plays: 3,
        loved: 0,
        rating: None,
        key: format!("{release_id}"),
        last_played: None,
        skips: 0,
    }
}

/// Cora Seed's 2001 and Ann Example's 1003, the seeds of the features tests.
fn seeds() -> Profile {
    Profile {
        albums: vec![
            album("Cora Seed", "Seed Album", 2001),
            album("Ann Example", "Untitled", 1003),
        ],
    }
}

/// How far a test's setup goes.
#[derive(PartialEq, PartialOrd)]
enum Setup {
    /// A config and nothing else.
    Nothing,
    /// The fixture set imported.
    Catalog,
    /// And the seeds kept as the profile.
    Profile,
    /// And the whole library's list scored.
    Scored,
}

/// A server for `test` set up as far as `setup`, and its data directory.
fn server(test: &str, setup: &Setup) -> (SocketAddr, PathBuf) {
    server_slots(test, setup, SeedSlots::new(api::SEED_REQUESTS))
}

/// `server`, counting the requests that compute or wait for a seed list in `slots`.
fn server_slots(test: &str, setup: &Setup, slots: SeedSlots) -> (SocketAddr, PathBuf) {
    let path = config_file(test);
    let config = config::load(&path).unwrap();
    if *setup >= Setup::Catalog {
        import::run(&config, &FakeClock::new(), import::BATCH, &mut |_| {}).unwrap();
    }
    if *setup >= Setup::Profile {
        profile::keep(&seeds(), &config.data_dir).unwrap();
    }
    if *setup >= Setup::Scored {
        rescore::run(&config, &FakeClock::new(), &mut |_| {}).unwrap();
    }
    let data_dir = config.data_dir.clone();
    let addr = client::serve_slots(
        config,
        api::LIMITS,
        Arc::new(SystemClock),
        Requests::default(),
        slots,
    );
    (addr, data_dir)
}

fn ask(addr: SocketAddr, body: &Value) -> Reply {
    send(addr, &post(PATH, &body.to_string()))
}

fn ok(addr: SocketAddr, body: &Value) -> Value {
    let reply = ask(addr, body);
    assert_eq!(
        (reply.status, &reply.body["ok"]),
        (200, &json!(true)),
        "{reply:?}"
    );
    reply.body
}

/// The kinds and ids of a reply's albums, in order.
fn ids(reply: &Value) -> Vec<(String, u64)> {
    reply["albums"]
        .as_array()
        .unwrap()
        .iter()
        .map(|album| {
            (
                album["kind"].as_str().unwrap().to_string(),
                album["id"].as_u64().unwrap(),
            )
        })
        .collect()
}

fn keys(value: &Value) -> BTreeSet<&str> {
    value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect()
}

const ALBUM_KEYS: [&str; 12] = [
    "kind",
    "id",
    "release_id",
    "rank",
    "score",
    "artists",
    "title",
    "year",
    "label",
    "styles",
    "community",
    "reasons",
];

const REASON_KEYS: [&str; 12] = [
    "kind",
    "words",
    "weight",
    "artist",
    "role",
    "seed_roles",
    "label",
    "via",
    "relation",
    "of",
    "albums",
    "styles",
];

#[test]
fn the_whole_library_s_albums_come_a_page_at_a_time_in_the_contract_s_shape() {
    let (addr, data_dir) = server("library", &Setup::Scored);
    // Gus Other's 2003 has community values; the rest have none yet.
    let mut db = Community::open(&data_dir).unwrap();
    db.queue_releases(&[2003], Tier::Ratings).unwrap();
    let values = Values {
        rating_average: 4.25,
        rating_count: 87,
        have: 1203,
        want: 340,
        num_for_sale: Some(12),
        lowest_price: None,
    };
    db.record(2003, values, "{}", 1_794_060_000).unwrap();
    drop(db);
    let body = ok(addr, &json!({"list": "library", "show": "albums"}));
    assert_eq!(
        keys(&body),
        BTreeSet::from(["ok", "list", "albums", "artists", "more"])
    );
    assert_eq!(body["artists"], Value::Null);
    assert_eq!(body["more"], false);
    let list = &body["list"];
    assert_eq!(list["catalog_set"], "20000101");
    assert_eq!(list["seeds"], 2);
    assert_eq!(
        keys(list),
        BTreeSet::from(["catalog_set", "computed_at", "seeds", "candidates"])
    );
    let albums = body["albums"].as_array().unwrap();
    assert_eq!(list["candidates"], albums.len());
    for album in albums {
        assert_eq!(keys(album), BTreeSet::from(ALBUM_KEYS), "{album}");
        for reason in album["reasons"].as_array().unwrap() {
            assert_eq!(keys(reason), BTreeSet::from(REASON_KEYS), "{reason}");
            assert!(reason["words"].as_str().is_some_and(|w| !w.is_empty()));
        }
    }
    let gus = albums.iter().find(|album| album["id"] == 2003).unwrap();
    assert_eq!(gus["kind"], "release");
    assert_eq!(gus["release_id"], 2003);
    assert_eq!(gus["title"], "Produced Album");
    assert_eq!(gus["year"], 2001);
    assert_eq!(gus["artists"], json!([{"id": 23, "name": "Gus Other"}]));
    assert_eq!(gus["label"], json!({"id": 30, "name": "Seed Records"}));
    assert_eq!(gus["styles"][0]["name"], "Made-Up Common");
    // The community values in `/v1/community`'s row.
    assert_eq!(
        gus["community"],
        json!({"release_id": 2003, "status": "fetched", "checked_at": 1_794_060_000,
               "values": {"rating_average": 4.25, "rating_count": 87, "have": 1203,
                          "want": 340, "num_for_sale": 12, "lowest_price": null}})
    );
    let producer = gus["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .find(|reason| reason["kind"] == "credit")
        .unwrap();
    assert_eq!(
        producer["words"],
        "Producer: Dee Producer, Producer on 1 album you have"
    );
    assert_eq!(
        producer["artist"],
        json!({"id": 21, "name": "Dee Producer"})
    );
    assert_eq!(producer["role"], "Producer");
    assert_eq!(producer["seed_roles"], json!(["Producer"]));
    assert_eq!(producer["albums"], 1);
    assert_eq!(producer["label"], Value::Null);
    // Another album has no values yet.
    let other = albums.iter().find(|album| album["id"] != 2003).unwrap();
    assert_eq!(other["community"]["status"], "not_queued");

    // Pages of one: each the next, `more` until the last; the rank is the stored list's.
    let all = ids(&body);
    let mut paged = Vec::new();
    for offset in 0..all.len() {
        let page = ok(
            addr,
            &json!({"list": "library", "show": "albums", "offset": offset, "limit": 1}),
        );
        assert_eq!(page["more"], offset + 1 < all.len(), "{offset}");
        paged.extend(ids(&page));
    }
    assert_eq!(paged, all);
    let first = ok(
        addr,
        &json!({"list": "library", "show": "albums", "limit": 1}),
    );
    assert_eq!(first["albums"][0]["rank"], 1);
}

#[test]
fn a_sort_by_owners_puts_the_most_owned_first_and_keeps_the_rank() {
    let (addr, data_dir) = server("sort", &Setup::Scored);
    let all_body = ok(addr, &json!({"list": "library", "show": "albums"}));
    let all = ids(&all_body);
    let albums = all_body["albums"].as_array().unwrap();
    // The first by score has some owners, the last by score more, and the rest none.
    let first = albums[0]["release_id"].as_u64().unwrap();
    let last_album = albums.last().unwrap();
    let last_release = last_album["release_id"].as_u64().unwrap();
    assert_ne!(first, last_release);
    let mut db = Community::open(&data_dir).unwrap();
    db.queue_releases(&[first, last_release], Tier::Ratings)
        .unwrap();
    for (id, have) in [(first, 10), (last_release, 5000)] {
        let values = Values {
            rating_average: 4.0,
            rating_count: 10,
            have,
            want: 1,
            num_for_sale: None,
            lowest_price: None,
        };
        db.record(id, values, "{}", 1_794_060_000).unwrap();
    }
    drop(db);
    let by_have = ok(
        addr,
        &json!({"list": "library", "show": "albums", "sort": "have"}),
    );
    let mut expected = vec![all[all.len() - 1].clone(), all[0].clone()];
    expected.extend(all[1..all.len() - 1].iter().cloned());
    assert_eq!(ids(&by_have), expected);
    assert_eq!(by_have["albums"][0]["rank"], last_album["rank"]);
    assert_eq!(by_have["more"], false);
    // A sort by score is the order without one.
    let by_score = ok(
        addr,
        &json!({"list": "library", "show": "albums", "sort": "score"}),
    );
    assert_eq!(ids(&by_score), all);
}

#[test]
fn the_filters_keep_what_they_ask_for() {
    let (addr, _) = server("filters", &Setup::Scored);
    let all = ok(addr, &json!({"list": "library", "show": "albums"}));
    let albums = all["albums"].as_array().unwrap();
    let kept = |body: Value| ids(&ok(addr, &body));
    let expected = |keep: &dyn Fn(&Value) -> bool| {
        let albums: Vec<Value> = albums.iter().filter(|album| keep(album)).cloned().collect();
        assert_ne!(albums.len(), 0);
        ids(&json!({ "albums": albums }))
    };
    assert_eq!(
        kept(json!({"list": "library", "show": "albums", "label_ids": [30]})),
        expected(&|album| album["label"]["id"] == 30)
    );
    assert_eq!(
        kept(json!({"list": "library", "show": "albums", "styles": ["made-up common"]})),
        expected(&|album| {
            album["styles"]
                .as_array()
                .unwrap()
                .iter()
                .any(|style| style["name"] == "Made-Up Common")
        })
    );
    assert_eq!(
        kept(json!({"list": "library", "show": "albums", "year_from": 2001, "year_to": 2003})),
        expected(&|album| album["year"]
            .as_u64()
            .is_some_and(|year| (2001..=2003).contains(&year)))
    );
    assert_eq!(
        kept(json!({"list": "library", "show": "albums", "year_from": 2002})),
        expected(&|album| album["year"].as_u64().is_some_and(|year| year >= 2002))
    );
    // The missing albums of the library's artists: Cora Seed's.
    let missing = kept(json!({"list": "library", "show": "albums", "missing": true}));
    assert_eq!(
        missing,
        [("release".to_string(), 2002), ("release".to_string(), 2011)]
    );
}

#[test]
fn a_marked_album_is_left_out_at_once() {
    let (addr, _) = server("marked", &Setup::Scored);
    let library = json!({"list": "library", "show": "albums"});
    assert!(ids(&ok(addr, &library)).contains(&("release".to_string(), 2003)));
    let mark = json!({"kind": "release", "id": 2003, "verdict": "not_interested"});
    let reply = send(addr, &post("/v1/feedback", &mark.to_string()));
    assert_eq!(reply.body["ok"], true, "{reply:?}");
    assert!(!ids(&ok(addr, &library)).contains(&("release".to_string(), 2003)));
}

#[test]
fn a_seed_s_albums_and_artists_and_the_whole_library_s_artists() {
    let (addr, data_dir) = server("seeds", &Setup::Scored);
    let cora = ok(
        addr,
        &json!({"list": "album", "release_id": 2001, "show": "albums"}),
    );
    let cora_ids = ids(&cora);
    assert!(cora_ids.contains(&("release".to_string(), 2003)), "{cora}");
    assert!(!cora_ids.contains(&("release".to_string(), 2008)), "{cora}");
    assert_eq!(cora["list"]["seeds"], 1);
    assert!(data_dir.join("seed-lists/album-2001.sqlite").exists());
    let by_name = ok(
        addr,
        &json!({"list": "artist", "artist": "cora seed", "show": "albums"}),
    );
    assert_eq!(ids(&by_name), cora_ids);

    let artists = ok(addr, &json!({"list": "library", "show": "artists"}));
    assert_eq!(artists["albums"], Value::Null);
    let rows = artists["artists"].as_array().unwrap();
    let gus = rows.iter().find(|artist| artist["id"] == 23).unwrap();
    assert_eq!(
        keys(gus),
        BTreeSet::from(["rank", "id", "name", "score", "albums", "best"])
    );
    assert_eq!(gus["name"], "Gus Other");
    assert_eq!(gus["albums"], 3);
    assert_eq!(keys(&gus["best"]), BTreeSet::from(ALBUM_KEYS));
    assert_eq!(rows[0]["rank"], 1);
    assert_eq!(rows[0]["score"], 1.0);
    let second = ok(
        addr,
        &json!({"list": "library", "show": "artists", "offset": 1, "limit": 1}),
    );
    assert_eq!(second["artists"][0]["id"], rows[1]["id"]);
    assert_eq!(second["artists"][0]["rank"], 2);
}

/// The `error.code` of a refusal, with its 200.
fn refused(addr: SocketAddr, body: &Value) -> (String, Value) {
    let reply = ask(addr, body);
    assert_eq!(reply.status, 200, "{reply:?}");
    (reply.code().to_string(), reply.body["error"].clone())
}

#[test]
fn what_can_t_be_listed_is_refused_with_its_code() {
    let library = json!({"list": "library", "show": "albums"});
    let (addr, _) = server("no-catalog", &Setup::Nothing);
    assert_eq!(refused(addr, &library).0, "no_catalog");
    let (addr, _) = server("no-profile", &Setup::Catalog);
    assert_eq!(refused(addr, &library).0, "no_profile");
    let (addr, _) = server("no-list", &Setup::Profile);
    assert_eq!(refused(addr, &library).0, "no_list");
    // A seed list doesn't read the stored list.
    ok(
        addr,
        &json!({"list": "album", "release_id": 2001, "show": "albums"}),
    );
    let (code, error) = refused(
        addr,
        &json!({"list": "album", "release_id": 9999, "show": "albums"}),
    );
    assert_eq!(code, "no_seed");
    assert!(
        error["message"].as_str().unwrap().contains("9999"),
        "{error}"
    );
    let (code, _) = refused(
        addr,
        &json!({"list": "artist", "artist": "Nobody", "show": "artists"}),
    );
    assert_eq!(code, "no_seed");

    let (addr, data_dir) = server("out-of-date", &Setup::Scored);
    let mut changed = seeds();
    changed.albums[0].plays += 1;
    profile::keep(&changed, &data_dir).unwrap();
    let (code, error) = refused(addr, &library);
    assert_eq!(code, "out_of_date");
    assert_eq!(error["rebuilding"], false);
}

fn bad_request(addr: SocketAddr, body: &Value) -> String {
    let reply = ask(addr, body);
    assert_eq!((reply.status, reply.code()), (400, "bad_request"), "{body}");
    reply.body["error"]["message"].as_str().unwrap().to_string()
}

#[test]
fn a_request_outside_the_shape_is_a_400_naming_the_field() {
    let (addr, _) = server("bad", &Setup::Nothing);
    let cases = [
        (json!({"show": "albums"}), "`list`"),
        (json!({"list": "library"}), "`show`"),
        (json!({"list": "everything", "show": "albums"}), "list"),
        (json!({"list": "library", "show": "tracks"}), "show"),
        (
            json!({"list": "library", "show": "albums", "extra": 1}),
            "`extra`",
        ),
        (json!({"list": "album", "show": "albums"}), "`release_id`"),
        (
            json!({"list": "library", "release_id": 2001, "show": "albums"}),
            "release_id",
        ),
        (json!({"list": "artist", "show": "albums"}), "`artist`"),
        (
            json!({"list": "album", "release_id": 0, "show": "albums"}),
            "release_id",
        ),
        (
            json!({"list": "library", "show": "albums", "limit": 0}),
            "limit",
        ),
        (
            json!({"list": "library", "show": "albums", "limit": 101}),
            "limit",
        ),
        (
            json!({"list": "library", "show": "albums", "offset": -1}),
            "offset",
        ),
        (
            json!({"list": "library", "show": "albums", "missing": "yes"}),
            "missing",
        ),
        (
            json!({"list": "library", "show": "albums", "styles": "House"}),
            "styles",
        ),
        (
            json!({"list": "library", "show": "albums", "label_ids": [0]}),
            "label_ids[0]",
        ),
        (
            json!({"list": "library", "show": "albums", "year_from": 2000, "year_to": 1999}),
            "year_from",
        ),
        (
            json!({"list": "library", "show": "albums", "year_to": 100_000}),
            "year_to",
        ),
        // The artists' list takes no filter.
        (
            json!({"list": "library", "show": "artists", "styles": []}),
            "styles",
        ),
        (
            json!({"list": "library", "show": "artists", "missing": false}),
            "missing",
        ),
        (
            json!({"list": "library", "show": "albums", "sort": "rank"}),
            "sort",
        ),
        (
            json!({"list": "library", "show": "albums", "sort": 1}),
            "sort",
        ),
        (
            json!({"list": "library", "show": "artists", "sort": "score"}),
            "sort",
        ),
    ];
    for (body, names) in cases {
        let message = bad_request(addr, &body);
        assert!(message.contains(names), "{body}: {message}");
    }
}

/// How long a test waits for what it expects, so a missing one fails it.
const WAIT: Duration = Duration::from_secs(10);

#[test]
fn requests_waiting_for_a_seed_list_are_bounded_and_the_rest_of_the_server_answers() {
    let slots = SeedSlots::new(api::SEED_REQUESTS);
    let (addr, data_dir) = server_slots("seed-bound", &Setup::Scored, slots.clone());
    // Album 2001's list is kept; 1003's is not.
    ok(
        addr,
        &json!({"list": "album", "release_id": 2001, "show": "albums"}),
    );
    assert_eq!(slots.taken(), 0);
    // A computation, in this process or another, holds the lock.
    let computing = fs::File::create(data_dir.join("seed-lists.lock")).unwrap();
    computing.lock().unwrap();
    let unkept = json!({"list": "album", "release_id": 1003, "show": "albums"});
    let (done, finished) = mpsc::channel();
    for _ in 0..api::SEED_REQUESTS {
        let (done, unkept) = (done.clone(), unkept.clone());
        thread::spawn(move || done.send(ask(addr, &unkept).status).unwrap());
    }
    let deadline = std::time::Instant::now() + WAIT;
    while slots.taken() < api::SEED_REQUESTS {
        assert!(
            std::time::Instant::now() < deadline,
            "the waiters never took the slots"
        );
        thread::yield_now();
    }
    // One more is refused at once, without waiting for the lock.
    let (refused, refusal) = mpsc::channel();
    thread::spawn(move || refused.send(ask(addr, &unkept)).unwrap());
    let reply = refusal
        .recv_timeout(WAIT)
        .expect("the fifth request waited");
    assert_eq!((reply.status, reply.code()), (503, "busy"), "{reply:?}");
    // A kept list's page and a ping answer meanwhile.
    ok(
        addr,
        &json!({"list": "album", "release_id": 2001, "show": "albums"}),
    );
    let ping = send(addr, &post("/v1/ping", "{}"));
    assert_eq!(ping.status, 200, "{ping:?}");
    // The waiters take their turns once the lock is let go, and give the slots back.
    assert!(finished.try_recv().is_err());
    computing.unlock().unwrap();
    for _ in 0..api::SEED_REQUESTS {
        assert_eq!(finished.recv_timeout(WAIT), Ok(200));
    }
    assert_eq!(slots.taken(), 0);
}
