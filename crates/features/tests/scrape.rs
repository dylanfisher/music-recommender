//! What the sync queues after the library (ADR 0030), on the catalog imported from the
//! sources crate's fixture set and the list a rebuild stores, against the stub Discogs API
//! with a made-up clock: the order the stub is asked in is the queue's.

#[path = "support/catalog.rs"]
mod catalog_support;
#[path = "support/clock.rs"]
mod clock;
#[path = "../../sources/tests/support/stub.rs"]
mod stub;
#[path = "../../config/tests/support/setup.rs"]
mod support;

use catalog::community::{Community, Request, Tier, Values};
use catalog_support::tagged;
use clock::FakeClock;
use config::Config;
use features::candidate::Candidate;
use features::profile::{self, Album, Profile};
use features::{rescore, sync};
use sources::discogs::api::Client;
use stub::{Stub, reply};

/// A list page of no rows, the list's only one.
const EMPTY_ARTIST_PAGE: &str = r#"{"pagination": {"pages": 1}, "releases": []}"#;
const EMPTY_MASTER_PAGE: &str = r#"{"pagination": {"pages": 1}, "versions": []}"#;

/// The fixture catalog in a fresh data directory named for the test, with `albums` kept and
/// queued as the library's and the list rebuilt from them, as `profile load` does.
fn library(test: &str, albums: Vec<Album>) -> Config {
    let config = catalog_support::imported_config("scrape", test, catalog_support::fixture_set);
    let profile = Profile { albums };
    profile::keep(&profile, &config.data_dir).unwrap();
    profile::queue(&profile, &config).unwrap();
    rescore::run(&config, &FakeClock::new(), &mut |_| {}).unwrap();
    config
}

/// A stub answering a release with the stub's release and a list with an empty page.
fn stub() -> Stub {
    Stub::start(|request| {
        let body = if request.path.starts_with("/artists/") {
            EMPTY_ARTIST_PAGE
        } else if request.path.starts_with("/masters/") {
            EMPTY_MASTER_PAGE
        } else {
            stub::RELEASE_JSON
        };
        reply(200, &[], body)
    })
}

/// Runs the sync until nothing is due, and returns the paths it asked for, without a list's
/// query.
fn asked(config: &Config, stub: &Stub, clock: &FakeClock) -> Vec<String> {
    let client = Client::new(&stub.url, support::TOKEN);
    sync::run(config, &client, clock, None, &mut |_| {}).unwrap();
    stub.received()
        .into_iter()
        .map(|request| request.path.split('?').next().unwrap().to_string())
        .collect()
}

#[test]
fn the_sweep_queues_the_library_s_artists_their_neighbors_and_masters_after_the_library() {
    // Ann Example's album, played, and Cora Seed's.
    let config = library("sweep", vec![tagged(1003, 10), tagged(2001, 0)]);
    let stub = stub();
    assert_eq!(
        asked(&config, &stub, &FakeClock::new()),
        [
            "/releases/1003",
            "/releases/2001",
            // Ann Example, then her group and her alias at three quarters of her weight,
            // then Cora Seed, then the two producers credited on Cora Seed's album.
            "/artists/2/releases",
            "/artists/1/releases",
            "/artists/4/releases",
            "/artists/20/releases",
            "/artists/21/releases",
            "/artists/22/releases",
            // The candidate master with two pressings.
            "/masters/100/versions",
            // Then the candidates' representatives, by score: no list named any.
            "/releases/1001",
            "/releases/2003",
            "/releases/2008",
            "/releases/2005",
            "/releases/2002",
            "/releases/2011",
            "/releases/2004",
        ]
    );
}

#[test]
fn a_list_is_swept_once_and_a_sync_without_a_catalog_fetches_the_library() {
    let config = library("sweep-once", vec![tagged(2001, 0)]);
    let stub = stub();
    let clock = FakeClock::new();
    assert!(asked(&config, &stub, &clock).len() > 1);
    // A second run finds the same list swept.
    assert_eq!(asked(&config, &stub, &clock), Vec::<String>::new());

    let (path, _) = support::write_config(&support::fresh_dir("scrape", "no-catalog"));
    let config = config::load(&path).unwrap();
    profile::queue(
        &Profile {
            albums: vec![tagged(2001, 0)],
        },
        &config,
    )
    .unwrap();
    assert_eq!(asked(&config, &stub, &clock), ["/releases/2001"]);
}

#[test]
fn a_rebuild_after_the_sweep_makes_a_master_s_most_owned_version_its_representative() {
    let albums = |plays| vec![tagged(1003, plays), tagged(2001, 0)];
    let config = library("most-owned", albums(10));
    // Master 100's reissue is owned ten times as often as its main release.
    let stub = Stub::start(|request| {
        let body = if request.path.starts_with("/masters/100/") {
            r#"{"pagination": {"pages": 1}, "versions": [
                {"id": 1001, "stats": {"community": {"in_collection": 5, "in_wantlist": 1}}},
                {"id": 1002, "stats": {"community": {"in_collection": 50, "in_wantlist": 2}}}
            ]}"#
        } else if request.path.starts_with("/artists/") {
            EMPTY_ARTIST_PAGE
        } else {
            stub::RELEASE_JSON
        };
        reply(200, &[], body)
    });
    let representative = |config: &Config| {
        catalog_support::stored(&config.data_dir, u64::MAX)
            .unwrap()
            .candidates
            .into_iter()
            .find(|listed| listed.scored.candidate == Candidate::Master(100))
            .unwrap()
            .representative
    };
    assert_eq!(representative(&config), Some(1001));
    asked(&config, &stub, &FakeClock::new());
    // The next rebuild, after the player sends a play more.
    profile::keep(&Profile { albums: albums(11) }, &config.data_dir).unwrap();
    rescore::run(&config, &FakeClock::new(), &mut |_| {}).unwrap();
    assert_eq!(representative(&config), Some(1002));
}

/// Records `values` for release `id` as an earlier batch's reply.
fn fetched(config: &Config, id: u64, values: Values) {
    let mut db = Community::open(&config.data_dir).unwrap();
    db.queue(&[(Request::release(id), 0.0)], Tier::Ratings)
        .unwrap();
    db.record(id, values, "{}", clock::START).unwrap();
}

/// The paths asked for after the library and the sweep, which are the first `skip`.
fn ratings(paths: &[String], skip: usize) -> Vec<&str> {
    paths[skip..].iter().map(String::as_str).collect()
}

/// Ann Example's album, played, and Cora Seed's: 2 library releases, 6 artists, 1 master.
const LIBRARY_AND_SWEEP: usize = 9;

#[test]
fn a_well_rated_reply_raises_its_neighbors_and_a_dead_cluster_sinks() {
    let config = library("neighbors", vec![tagged(1003, 10), tagged(2001, 0)]);
    // Cora Seed's Second Album is well rated and well owned...
    let well = Values {
        rating_average: 4.5,
        rating_count: 80,
        have: 900,
        want: 300,
        num_for_sale: None,
        lowest_price: None,
    };
    fetched(&config, 2002, well);
    // ...and Gus Other's Produced Album has almost no owners or votes.
    let dead = Values {
        rating_average: 0.0,
        rating_count: 0,
        have: 1,
        want: 0,
        ..well
    };
    fetched(&config, 2003, dead);
    let paths = asked(&config, &stub(), &FakeClock::new());
    assert_eq!(
        ratings(&paths, LIBRARY_AND_SWEEP),
        [
            "/releases/1001",
            "/releases/2008",
            // Cora Seed's other album, at twice its score, passes Gus Other's two, which
            // sink to a quarter of theirs: by score alone it would be 2005, 2011, 2004.
            "/releases/2011",
            "/releases/2005",
            "/releases/2004",
        ]
    );
}

#[test]
fn the_next_batch_is_queued_once_the_one_before_is_fetched() {
    let config = library("batches", vec![tagged(1003, 10), tagged(2001, 0)]);
    // A batch in progress, its last request not yet sent, at a priority below every
    // candidate's.
    Community::open(&config.data_dir)
        .unwrap()
        .queue(&[(Request::release(9999), 0.0)], Tier::Ratings)
        .unwrap();
    let paths = asked(&config, &stub(), &FakeClock::new());
    let rest = ratings(&paths, LIBRARY_AND_SWEEP);
    assert_eq!(rest[0], "/releases/9999");
    assert_eq!(rest.len(), 8, "{rest:?}");
}
