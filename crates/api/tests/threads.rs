//! `POST /v1/threads` over real `TcpStream`s on `127.0.0.1`, against the sources crate's
//! fixture set imported, with a made-up clock: one seed's threads in the shape docs/api.md
//! writes, the refusals and the 400s, and no seed list's slot taken. The budget is measured
//! on ADR 0022's generated set.

#[path = "support/client.rs"]
mod client;
#[path = "../../features/tests/support/clock.rs"]
mod clock;
#[path = "../../features/tests/support/generated.rs"]
mod generated;
#[path = "../../config/tests/support/setup.rs"]
mod support;

use std::fs;
use std::io::Write as _;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use client::{Reply, post, send};
use clock::{FakeClock, START};
use features::import;
use features::list::SeedSlots;
use features::profile::{self, Album, Profile};
use features::rescore::Requests;
use serde_json::{Value, json};

const PATH: &str = "/v1/threads";
const DAY: i64 = 86_400;

fn album(key: &str, artist: &str, title: &str, release_id: Option<u64>) -> Album {
    Album {
        key: key.to_string(),
        artist: artist.to_string(),
        title: title.to_string(),
        track_count: 1,
        durations: vec![180],
        catalog_numbers: Vec::new(),
        barcodes: Vec::new(),
        discogs_release_id: release_id,
        plays: 0,
        loved: 0,
        rating: None,
        last_played: None,
        skips: 0,
    }
}

/// Cora Seed's 2001, played 3 days before the clock's start; Gus Other's 2003, which Cora
/// Seed sings on, and his 2005 on Seed Records' sublabel; an untagged album; one tagged
/// with a release the catalog lacks; and 2014, a Various album on Seed Records.
fn library() -> Profile {
    Profile {
        albums: vec![
            Album {
                plays: 9,
                skips: 1,
                last_played: Some(START - 3 * DAY),
                ..album("a1", "Cora Seed", "Seed Album", Some(2001))
            },
            album("a2", "Gus Other", "Produced Album", Some(2003)),
            album("a3", "Gus Other", "Sub Album", Some(2005)),
            album("a4", "Made-Up Artist", "Untagged", None),
            album("a5", "Made-Up Artist", "Not In The Catalog", Some(9999)),
            album("a6", "Various", "Made-Up Hits", Some(2014)),
        ],
    }
}

/// A server for `test` with the fixture set imported when `catalog`, `profile` kept, and
/// `seed_slots` for the seed lists.
fn server(
    test: &str,
    catalog: bool,
    profile: Option<&Profile>,
    seed_slots: SeedSlots,
) -> SocketAddr {
    let dir = support::fresh_dir("threads", test);
    let (path, _) = support::write_config(&dir);
    if catalog {
        let dumps = dir.join("discogs");
        fs::create_dir_all(&dumps).unwrap();
        let fixtures =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../sources/tests/fixtures/dumps");
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
    }
    let config = config::load(&path).unwrap();
    if catalog {
        import::run(&config, &FakeClock::new(), import::BATCH, &mut |_| {}).unwrap();
    }
    if let Some(profile) = profile {
        profile::keep(profile, &config.data_dir).unwrap();
    }
    client::serve_slots(
        config,
        api::LIMITS,
        Arc::new(FakeClock::new()),
        Requests::default(),
        seed_slots,
    )
}

fn slots() -> SeedSlots {
    SeedSlots::new(api::SEED_REQUESTS)
}

fn call(addr: SocketAddr, body: &Value) -> Reply {
    send(addr, &post(PATH, &body.to_string()))
}

fn ok(addr: SocketAddr, body: &Value) -> Value {
    let reply = call(addr, body);
    assert_eq!(
        (reply.status, &reply.body["ok"]),
        (200, &json!(true)),
        "{reply:?}"
    );
    reply.body
}

/// `value`'s field names, sorted.
fn fields(value: &Value) -> Vec<&str> {
    let mut names: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    names.sort_unstable();
    names
}

#[test]
fn a_seed_s_threads_come_in_the_contract_s_shape() {
    let addr = server("shape", true, Some(&library()), slots());
    let reply = ok(addr, &json!({"key": "a2"}));
    assert_eq!(
        fields(&reply),
        ["albums", "computed_at", "ok", "seed", "threads"]
    );
    assert_eq!(reply["computed_at"], json!(START));
    let common = reply["seed"]["styles"][0]["id"].clone();
    assert_eq!(
        reply["seed"],
        json!({
            "id": "r2003", "key": "a2", "artist": "Gus Other", "title": "Produced Album",
            "release_id": 2003, "year": 2001, "label": {"id": 30, "name": "Seed Records"},
            "styles": [{"id": common, "name": "Made-Up Common"}],
            "plays": 0, "skips": 0, "last_played": null,
            "community": {"release_id": 2003, "status": "not_queued", "checked_at": null,
                          "values": null}
        })
    );
    assert_eq!(
        reply["threads"],
        json!([
            {"kind": "label", "words": "Seed Records, 2000 to 2001, on 2 albums you have",
             "ids": ["r2001", "r2014"], "artist": null,
             "label": {"id": 30, "name": "Seed Records"}},
            {"kind": "artist", "words": "By Gus Other, on 1 album you have", "ids": ["r2005"],
             "artist": {"id": 23, "name": "Gus Other"}, "label": null},
            {"kind": "credit", "words": "Vocals: Cora Seed, on 1 album you have",
             "ids": ["r2001"], "artist": {"id": 20, "name": "Cora Seed"}, "label": null}
        ])
    );
    let albums = reply["albums"].as_array().unwrap();
    let ids: Vec<&str> = albums.iter().map(|a| a["id"].as_str().unwrap()).collect();
    // Dee Producer is on 2001 too, but a thread of the same albums as Cora Seed's is left
    // out. Gus Other's 2005 is in his own thread only. 2001 was played this month and still
    // takes part.
    assert_eq!(ids, ["r2001", "r2014", "r2005"]);
    assert_eq!(fields(&albums[0]), fields(&reply["seed"]));
    assert_eq!(
        (
            &albums[0]["key"],
            &albums[0]["year"],
            &albums[0]["plays"],
            &albums[0]["last_played"]
        ),
        (
            &json!("a1"),
            &json!(2000),
            &json!(9),
            &json!(START - 3 * DAY)
        )
    );
    assert_eq!(albums[2]["label"], json!({"id": 31, "name": "Seed Sub"}));
}

#[test]
fn a_seed_that_can_t_have_threads_is_refused() {
    let refused = |addr, key: &str| {
        let reply = call(addr, &json!({ "key": key }));
        assert_eq!(
            (reply.status, &reply.body["ok"]),
            (200, &json!(false)),
            "{reply:?}"
        );
        reply.code().to_string()
    };
    let addr = server("no-profile", true, None, slots());
    assert_eq!(refused(addr, "a2"), "no_profile");
    let addr = server("refused", true, Some(&library()), slots());
    assert_eq!(refused(addr, "a7"), "no_seed");
    assert_eq!(refused(addr, "a4"), "untagged");
    assert_eq!(refused(addr, "a5"), "untagged");
    let addr = server("no-catalog", false, Some(&library()), slots());
    assert_eq!(refused(addr, "a2"), "no_catalog");
}

#[test]
fn threads_take_no_seed_list_s_slot() {
    // No slot at all: a seed list that isn't kept would be a 503.
    let addr = server("no-slots", true, Some(&library()), SeedSlots::new(0));
    assert_eq!(
        ok(addr, &json!({"key": "a2"}))["seed"]["id"],
        json!("r2003")
    );
}

#[test]
fn a_request_outside_the_shape_is_a_400_naming_the_field() {
    let addr = server("bad", false, Some(&library()), slots());
    for (body, field) in [
        (json!({}), "key"),
        (json!({"key": ""}), "key"),
        (json!({"key": "k".repeat(65)}), "key"),
        (json!({"key": 2}), "key"),
        (json!({"key": "a2", "limit": 5}), "limit"),
    ] {
        let reply = call(addr, &body);
        assert_eq!((reply.status, reply.code()), (400, "bad_request"), "{body}");
        let message = reply.body["error"]["message"].as_str().unwrap();
        assert!(message.contains(field), "{body}: {message}");
    }
    // 64 characters is a key, if not one of the profile's.
    let reply = call(addr, &json!({"key": "k".repeat(64)}));
    assert_eq!((reply.status, reply.code()), (200, "no_seed"));
}

/// The time and size of the threads of every tagged album of a library of the owner's size,
/// 6,500 albums, 4,000 of them tagged, on 1M generated releases, as Rediscover's budget
/// has it. Run with
/// `cargo test --release -p api --test threads -- --ignored budget --nocapture`.
#[test]
#[ignore = "a benchmark: generates and imports 1M releases"]
fn budget_of_threads_on_1m_generated_releases() {
    use std::time::{Duration, Instant};
    const RELEASES: u64 = 1_000_000;
    const TAGGED: u64 = 4000;
    const ALBUMS: u64 = 6500;
    let dir = support::fresh_dir("threads", "budget");
    let (path, _) = support::write_config(&dir);
    let dumps = dir.join("discogs");
    fs::create_dir_all(&dumps).unwrap();
    generated::write_set(&dumps, RELEASES);
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(format!("[dumps]\ndir = \"{}\"\n", dumps.display()).as_bytes())
        .unwrap();
    let config = config::load(&path).unwrap();
    import::run(&config, &FakeClock::new(), import::BATCH, &mut |_| {}).unwrap();
    let albums: Vec<Album> = (0..ALBUMS)
        .map(|n| {
            // The generator's release `id` is by artist `id % (RELEASES / 2) + 1`.
            let id = n * (RELEASES / TAGGED) + 1;
            Album {
                plays: n % 7 * 20,
                skips: n % 5,
                last_played: (n % 3 > 0).then(|| START - i64::try_from(n).unwrap() * 3600),
                ..album(
                    &format!("a{n}"),
                    &format!("Artist {}", id % (RELEASES / 2) + 1),
                    &format!("Generated Release Number {id}"),
                    (n < TAGGED).then_some(id),
                )
            }
        })
        .collect();
    profile::keep(&Profile { albums }, &config.data_dir).unwrap();
    let addr = client::serve_slots(
        config,
        api::LIMITS,
        Arc::new(FakeClock::new()),
        Requests::default(),
        slots(),
    );
    let started = Instant::now();
    ok(addr, &json!({"key": "a0"}));
    println!("first, the seeds unread: {:?}", started.elapsed());
    let mut times: Vec<Duration> = Vec::new();
    let mut largest = (0, String::new(), 0, 0);
    for n in 0..TAGGED {
        let key = format!("a{n}");
        let started = Instant::now();
        let reply = ok(addr, &json!({ "key": key }));
        times.push(started.elapsed());
        let bytes = reply.to_string().len();
        if bytes > largest.0 {
            let albums = reply["albums"].as_array().unwrap().len();
            let threads = reply["threads"].as_array().unwrap().len();
            largest = (bytes, key, albums, threads);
        }
    }
    times.sort_unstable();
    println!(
        "after, {} seeds: median {:?}, slowest {:?}",
        times.len(),
        times[times.len() / 2],
        times[times.len() - 1]
    );
    let (bytes, key, albums, threads) = largest;
    println!("largest reply: {key}, {bytes} bytes, {albums} albums in {threads} threads");
}
