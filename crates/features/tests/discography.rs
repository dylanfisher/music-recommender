//! An artist's discography on the catalog imported from the sources crate's fixture set: its
//! main-artist albums only, every format, owned and marked albums flagged, each order and its
//! ties, the rating's prior, and the sync's discography tier.

#[path = "support/catalog.rs"]
mod catalog_support;
#[path = "support/clock.rs"]
mod clock;
#[path = "../../config/tests/support/setup.rs"]
mod support;

use std::time::Duration;

use catalog::community::{Community, REFRESH_AFTER, Request, Tier};
use catalog::score::Filter;
use catalog_support::{album, fetched, values};
use clock::FakeClock;
use config::Config;
use features::candidate::Candidate;
use features::clock::Clock;
use features::discography::{self, Ask, Discography, Order};
use features::feedback::{self, Kind, Verdict};
use features::list::ListError;
use features::profile::{self, Profile};
use features::sort::Sort;

const THE_BAND: u64 = 1;
const ANN: u64 = 2;
const DJ_ANN: u64 = 4;
const CORA: u64 = 20;
const GUS: u64 = 23;
const VARIOUS: u64 = 194;

/// The fixture catalog in a fresh data directory named for the test, with a profile of
/// Cora Seed's 2001 and The Made-Up Band's 1002, a release of its master 100 that isn't the
/// main one.
fn config(test: &str) -> Config {
    let config =
        catalog_support::imported_config("discography", test, catalog_support::fixture_set);
    let albums = vec![
        album("Cora Seed", "Seed Album", Some(2001), 3),
        album("The Made-Up Band", "First Album", Some(1002), 1),
    ];
    profile::keep(&Profile { albums }, &config.data_dir).unwrap();
    config
}

/// `config`'s catalog with `sql` run on it.
fn changed(config: &Config, sql: &str) {
    rusqlite::Connection::open(config.data_dir.join("catalog.sqlite"))
        .unwrap()
        .execute_batch(sql)
        .unwrap();
}

fn ask(artist_id: u64, order: Order) -> Ask {
    Ask {
        artist_id,
        filter: Filter::default(),
        order,
        offset: 0,
        limit: 100,
    }
}

fn found(config: &Config, clock: &FakeClock, ask: &Ask) -> Discography {
    discography::discography(config, clock, ask).unwrap()
}

/// The ids of a discography's page, in order.
fn ids(found: &Discography) -> Vec<u64> {
    found
        .page
        .items
        .iter()
        .map(|album| album.candidate.kind_id().1)
        .collect()
}

/// The ids of `artist_id`'s discography in `order`.
fn ordered(config: &Config, artist_id: u64, order: Order) -> Vec<u64> {
    ids(&found(config, &FakeClock::new(), &ask(artist_id, order)))
}

#[test]
fn only_the_albums_the_artist_is_a_main_artist_of_are_listed() {
    let config = config("main");
    // Cora Seed is credited on Gus Other's 2003, and Ann Example's alias DJ Ann has 2008 and
    // her group the band's master 100: none of them is theirs.
    assert_eq!(ordered(&config, CORA, Order::Year), [2001, 2002, 2011]);
    assert_eq!(ordered(&config, ANN, Order::Year), [1003]);
    assert_eq!(ordered(&config, DJ_ANN, Order::Year), [2008]);
    // The band's master once, through its main release, though both its releases are the
    // band's.
    let band = found(&config, &FakeClock::new(), &ask(THE_BAND, Order::Year));
    let master = &band.page.items[0];
    assert_eq!(band.page.items.len(), 1);
    assert_eq!(master.candidate, Candidate::Master(100));
    assert_eq!(master.representative, Some(1001));
    assert_eq!(master.year, Some(1999));
    assert_eq!(band.artist.name.as_deref(), Some("The Made-Up Band"));
    assert_eq!(band.albums, 1);
    // A master Cora Seed is on only through one release of it isn't hers.
    changed(
        &config,
        "INSERT INTO release_artist (release_id, position, artist_id) VALUES (1002, 2, 20)",
    );
    assert_eq!(ordered(&config, CORA, Order::Year), [2001, 2002, 2011]);
    // Various is an artist like any other.
    assert_eq!(ordered(&config, VARIOUS, Order::Year), [2014]);
}

#[test]
fn every_format_is_listed_and_shown() {
    let config = config("formats");
    // Gus Other's 2004 on cassette, 2005 on CD.
    changed(
        &config,
        "INSERT INTO format (id, name) VALUES (90, 'Cassette');
         INSERT INTO description (id, name) VALUES (91, 'EP');
         INSERT INTO release_format (release_id, position, format_id, qty, descriptions)
         VALUES (2004, 1, 90, 1, NULL),
                (2005, 1, (SELECT id FROM format WHERE name = 'CD'), 1, X'5b000000');",
    );
    let gus = found(&config, &FakeClock::new(), &ask(GUS, Order::Year));
    let formats: Vec<(u64, Vec<String>)> = gus
        .page
        .items
        .iter()
        .map(|album| (album.candidate.kind_id().1, album.detail.formats.clone()))
        .collect();
    assert_eq!(
        formats,
        [
            (2003, vec![]),
            (2005, vec!["CD, EP".to_string()]),
            (2006, vec![]),
            (2004, vec!["Cassette".to_string()]),
        ]
    );
    let band = found(&config, &FakeClock::new(), &ask(THE_BAND, Order::Year));
    assert_eq!(
        band.page.items[0].detail.formats,
        ["Vinyl, LP, Album", "CD, Album"]
    );
}

#[test]
fn owned_and_marked_albums_are_listed_and_flagged() {
    let config = config("flags");
    feedback::set(&config, Kind::Release, 2002, Verdict::NotInterested, 0).unwrap();
    feedback::set(&config, Kind::Release, 1001, Verdict::AlreadyHave, 0).unwrap();
    let cora = found(&config, &FakeClock::new(), &ask(CORA, Order::Year));
    let flags: Vec<(u64, bool, Option<Verdict>)> = cora
        .page
        .items
        .iter()
        .map(|album| {
            (
                album.candidate.kind_id().1,
                album.detail.in_library,
                album.detail.mark,
            )
        })
        .collect();
    assert_eq!(
        flags,
        [
            (2001, true, None),
            (2002, false, Some(Verdict::NotInterested)),
            (2011, false, None),
        ]
    );
    // The library has 1002, a release of the master; the verdict is on its main release.
    let band = found(&config, &FakeClock::new(), &ask(THE_BAND, Order::Year));
    let master = &band.page.items[0];
    assert!(master.detail.in_library);
    assert_eq!(master.detail.mark, Some(Verdict::AlreadyHave));
}

#[test]
fn year_and_title_order_with_ties_by_year_then_title_then_id() {
    let config = config("year-title");
    // Gus Other's years: 2003 in 2001, 2005 in 2003, 2006 in 2004, 2004 none.
    assert_eq!(ordered(&config, GUS, Order::Year), [2003, 2005, 2006, 2004]);
    // Cora Seed's: 2001 in 2000, the others none, so by title.
    assert_eq!(ordered(&config, CORA, Order::Year), [2001, 2002, 2011]);
    // "Second Album", "Seed Album", "Third Album", ignoring case.
    changed(
        &config,
        "UPDATE release SET title = 'seed album' WHERE id = 2001",
    );
    assert_eq!(ordered(&config, CORA, Order::Title), [2002, 2001, 2011]);
    // The same title: by year, then id.
    changed(
        &config,
        "UPDATE release SET title = 'Same' WHERE id IN (2003, 2004, 2005)",
    );
    assert_eq!(
        ordered(&config, GUS, Order::Title),
        [2006, 2003, 2005, 2004]
    );
    let gus = found(&config, &FakeClock::new(), &ask(GUS, Order::Title));
    let ranks: Vec<u64> = gus.page.items.iter().map(|album| album.rank).collect();
    assert_eq!(ranks, [1, 2, 3, 4]);
}

#[test]
fn a_community_sort_orders_every_album_with_the_unrated_last_by_year() {
    let config = config("values");
    fetched(
        &config.data_dir,
        &[
            (2004, values(4.0, 10, 10, 3)),
            (2006, values(3.0, 10, 5000, 1)),
        ],
    );
    let by = |sort| ordered(&config, GUS, Order::Value(sort));
    assert_eq!(by(Sort::Have), [2006, 2004, 2003, 2005]);
    assert_eq!(by(Sort::Want), [2004, 2006, 2003, 2005]);
    // A tie keeps the year order.
    fetched(&config.data_dir, &[(2005, values(4.0, 10, 5000, 1))]);
    assert_eq!(by(Sort::Have), [2005, 2006, 2004, 2003]);
}

#[test]
fn the_rating_is_shrunk_toward_the_mean_of_every_vote_in_the_database() {
    let config = config("prior");
    // 2003's 5.0 from one vote beats 2005's 3.0 from 100 against their own mean (3.02), but
    // the database's, near 1.0 with the band's 10,000 votes, holds it back.
    fetched(
        &config.data_dir,
        &[
            (1001, values(1.0, 10_000, 1, 1)),
            (2003, values(5.0, 1, 1, 1)),
            (2005, values(3.0, 100, 1, 1)),
        ],
    );
    assert_eq!(
        ordered(&config, GUS, Order::Value(Sort::Rating)),
        [2005, 2003, 2006, 2004]
    );
}

#[test]
fn the_filters_keep_what_they_keep_in_a_scored_list_and_the_rank_is_before_them() {
    let config = config("filters");
    let kept = |filter: Filter| {
        let found = found(
            &config,
            &FakeClock::new(),
            &Ask {
                filter,
                ..ask(GUS, Order::Year)
            },
        );
        let ranks: Vec<(u64, u64)> = found
            .page
            .items
            .iter()
            .map(|album| (album.candidate.kind_id().1, album.rank))
            .collect();
        (ranks, found.albums)
    };
    assert_eq!(
        kept(Filter {
            label_ids: vec![31],
            ..Filter::default()
        }),
        (vec![(2005, 2), (2006, 3)], 4)
    );
    assert_eq!(
        kept(Filter {
            years: Some((2002, 2004)),
            ..Filter::default()
        }),
        (vec![(2005, 2), (2006, 3)], 4)
    );
    assert_eq!(
        kept(Filter {
            styles: vec!["made-up common".to_string()],
            ..Filter::default()
        })
        .0
        .len(),
        4
    );
    // A page of one after one, and whether more follow.
    let page = found(
        &config,
        &FakeClock::new(),
        &Ask {
            offset: 1,
            limit: 1,
            ..ask(GUS, Order::Year)
        },
    );
    assert_eq!((ids(&page), page.page.more), (vec![2005], true));
}

#[test]
fn an_artist_the_catalog_doesn_t_have_is_refused() {
    let config = config("unknown");
    let error =
        discography::discography(&config, &FakeClock::new(), &ask(999, Order::Year)).unwrap_err();
    assert!(matches!(error, ListError::UnknownArtist(999)), "{error}");
    assert!(error.to_string().contains("999"), "{error}");
}

/// The requests next in the queue at the fake clock's start, in order.
fn queued(config: &Config) -> Vec<(Request, Tier)> {
    Community::open(&config.data_dir)
        .unwrap()
        .queue_view(clock::START)
        .unwrap()
        .next
        .into_iter()
        .map(|(request, tier, _)| (request, tier))
        .collect()
}

#[test]
fn a_discography_queues_its_missing_values_after_the_library_the_latest_first() {
    let config = config("tier");
    let clock = FakeClock::new();
    assert_eq!(found(&config, &clock, &ask(CORA, Order::Year)).pending, 3);
    clock.sleep(Duration::from_secs(1));
    let gus = found(&config, &clock, &ask(GUS, Order::Year));
    assert_eq!(gus.pending, 4);
    let release = |id| (Request::release(id), Tier::Discography);
    // Gus Other's, asked last, before Cora Seed's, each in its order.
    assert_eq!(
        queued(&config),
        [
            release(2003),
            release(2005),
            release(2006),
            release(2004),
            release(2001),
            release(2002),
            release(2011),
        ]
    );
    // The count goes down as the values come.
    fetched(&config.data_dir, &[(2003, values(4.0, 1, 1, 1))]);
    assert_eq!(found(&config, &clock, &ask(GUS, Order::Year)).pending, 3);
    fetched(
        &config.data_dir,
        &[
            (2004, values(4.0, 1, 1, 1)),
            (2005, values(4.0, 1, 1, 1)),
            (2006, values(4.0, 1, 1, 1)),
        ],
    );
    assert_eq!(found(&config, &clock, &ask(GUS, Order::Year)).pending, 0);
    // Values over 90 days old are fetched again, in the tier.
    let later = FakeClock::new();
    later.sleep(REFRESH_AFTER + Duration::from_secs(1));
    assert_eq!(found(&config, &later, &ask(GUS, Order::Year)).pending, 4);
}

#[test]
fn the_tier_keeps_the_last_five_discographies() {
    let config = config("five");
    let clock = FakeClock::new();
    for artist_id in [THE_BAND, ANN, DJ_ANN, CORA, GUS, VARIOUS] {
        found(&config, &clock, &ask(artist_id, Order::Year));
        clock.sleep(Duration::from_secs(1));
    }
    // The band's, asked first, went to the asked lists' tier.
    let tiers = Community::open(&config.data_dir)
        .unwrap()
        .queue_view(clock::START)
        .unwrap()
        .tiers;
    assert_eq!(tiers[1..3], [(Tier::Discography, 10), (Tier::Asked, 1)]);
    let mut db = Community::open(&config.data_dir).unwrap();
    db.record(1003, values(4.0, 1, 1, 1), "{}", clock::START)
        .unwrap();
    assert_eq!(
        db.queue_view(clock::START).unwrap().next[9].0,
        Request::release(1001)
    );
}

#[test]
fn one_request_queues_at_most_500_releases() {
    use std::fmt::Write as _;
    let config = config("cap");
    // 501 releases of their own for Lone & Only.
    let mut sql = String::new();
    for id in 5000..5501 {
        let _ = write!(
            sql,
            "INSERT INTO release (id, title, master_id, is_main_release, tracks)
             VALUES ({id}, 'Album {id}', 0, 0, 1);
             INSERT INTO release_artist (release_id, position, artist_id) VALUES ({id}, 1, 5);"
        );
    }
    changed(&config, &sql);
    let lone = found(&config, &FakeClock::new(), &ask(5, Order::Year));
    assert_eq!((lone.albums, lone.pending), (501, 500));
    let tiers = Community::open(&config.data_dir)
        .unwrap()
        .queue_view(clock::START)
        .unwrap()
        .tiers;
    assert!(tiers.contains(&(Tier::Discography, 500)), "{tiers:?}");
}
