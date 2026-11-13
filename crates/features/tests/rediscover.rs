//! Rediscover on the catalog imported from the sources crate's fixture set: the day's albums
//! of the kept profile with what the catalog and the community database hold of them, and
//! without a catalog. The budget is measured on ADR 0022's generated set.

#[path = "support/catalog.rs"]
mod catalog_support;
#[path = "support/clock.rs"]
mod clock;
#[path = "support/generated.rs"]
mod generated;
#[path = "../../config/tests/support/setup.rs"]
mod support;

use catalog::community::Stored;
use catalog_support::{album, fetched, values};
use clock::FakeClock;
use config::Config;
use features::profile::{self, Album, Profile};
use features::rediscover::{self, Day, Id, Kept, Reason, Rediscover, RediscoverError};
use features::seed::Seeds;

/// The fixture catalog in a fresh data directory named for the test.
fn config(test: &str) -> Config {
    catalog_support::imported_config("rediscover", test, catalog_support::fixture_set)
}

/// A fresh data directory named for the test, with no catalog.
fn bare(test: &str) -> Config {
    let dir = support::fresh_dir("rediscover", test);
    let (path, _) = support::write_config(&dir);
    config::load(&path).unwrap()
}

/// The fixture's day.
fn day() -> Day {
    Day::parse("2026-11-12").unwrap()
}

/// `album` last played `days` days before the clock's start.
fn played(album: Album, days: i64) -> Album {
    Album {
        last_played: Some(clock::START - days * 86_400),
        plays: 4,
        ..album
    }
}

fn keep(config: &Config, albums: Vec<Album>) {
    profile::keep(&Profile { albums }, &config.data_dir).unwrap();
}

fn rediscover(config: &Config) -> Rediscover {
    rediscover::rediscover(config, &FakeClock::new(), &Kept::default(), &day()).unwrap()
}

#[test]
fn without_a_profile_there_is_nothing_to_rediscover() {
    let config = bare("no-profile");
    let error =
        rediscover::rediscover(&config, &FakeClock::new(), &Kept::default(), &day()).unwrap_err();
    assert!(matches!(error, RediscoverError::NoProfile), "{error}");
}

#[test]
fn the_day_s_albums_have_the_catalog_s_year_label_and_styles_and_their_values() {
    let config = config("catalog");
    let produced = album("Gus Other", "Produced Album", Some(2003), 0);
    let untagged = album("Made-Up Artist", "Untagged", None, 0);
    keep(
        &config,
        vec![
            // Played this month, so it takes no part.
            played(album("Cora Seed", "Seed Album", Some(2001), 3), 3),
            produced.clone(),
            played(untagged.clone(), 400),
        ],
    );
    fetched(&config.data_dir, &[(2003, values(4.4, 310, 5120, 1400))]);
    let found = rediscover(&config);
    assert_eq!(found.day, day());
    assert_eq!(found.computed_at, clock::START);
    assert_eq!(found.albums_considered, 3);
    let ids: Vec<String> = found.albums.iter().map(|a| a.id.to_string()).collect();
    assert_eq!(ids, ["r2003".to_string(), Id::of(&untagged).to_string()]);
    let first = &found.albums[0];
    assert_eq!(first.album, produced);
    assert_eq!(first.year, Some(2001));
    let label = first.label.as_ref().unwrap();
    assert_eq!(
        (label.id, label.name.as_deref()),
        (30, Some("Seed Records"))
    );
    let styles: Vec<_> = first.styles.iter().map(|s| s.name.as_deref()).collect();
    assert_eq!(styles, [Some("Made-Up Common")]);
    assert!(matches!(first.community, Some(Stored::Fetched { .. })));
    assert!((first.score - 1.0).abs() < 1e-12);
    assert_eq!(first.reasons[0], Reason::NeverPlayed);
    let second = &found.albums[1];
    assert_eq!((second.year, second.community), (None, None));
    assert!(second.label.is_none() && second.styles.is_empty());
    assert_eq!(second.reasons[0], Reason::NotPlayedSince(400));
    assert_eq!(second.reasons[0].words(), "Last played 1 year ago");
    assert!(second.score < first.score);
}

#[test]
fn without_a_catalog_the_albums_have_no_links_and_are_not_refused() {
    let config = bare("no-catalog");
    keep(
        &config,
        vec![album("Cora Seed", "Seed Album", Some(2001), 3)],
    );
    let found = rediscover(&config);
    assert_eq!(found.albums.len(), 1);
    let only = &found.albums[0];
    assert_eq!(only.id, Id::Release(2001));
    assert_eq!((only.year, &only.label), (None, &None));
    assert_eq!(only.community, Some(Stored::NotQueued));
}

/// The time Rediscover takes for a library of the owner's size, 6,500 albums, 4,000 of them
/// tagged, on 1M generated releases. Run with
/// `cargo test --release -p features --test rediscover -- --ignored budget --nocapture`.
#[test]
#[ignore = "a benchmark: generates and imports 1M releases"]
fn budget_of_a_day_on_1m_generated_releases() {
    use std::time::Instant;
    const RELEASES: u64 = 1_000_000;
    const TAGGED: u64 = 4000;
    const ALBUMS: u64 = 6500;
    let config = catalog_support::imported_config("rediscover", "budget", |dumps| {
        generated::write_set(dumps, RELEASES);
    });
    let albums: Vec<Album> = (0..ALBUMS)
        .map(|n| {
            // The generator's release `id` is by artist `id % (RELEASES / 2) + 1`.
            let id = n * (RELEASES / TAGGED) + 1;
            let tagged = n < TAGGED;
            Album {
                plays: n % 7 * 20,
                skips: n % 5,
                loved: u32::from(n % 11 == 0),
                last_played: (n % 3 > 0).then(|| clock::START - i64::try_from(n).unwrap() * 3600),
                ..album(
                    &format!("Artist {}", id % (RELEASES / 2) + 1),
                    &format!("Generated Release Number {id}"),
                    tagged.then_some(id),
                    0,
                )
            }
        })
        .collect();
    let with_values: Vec<_> = (0..TAGGED)
        .step_by(2)
        .map(|n| {
            let id = n * (RELEASES / TAGGED) + 1;
            (
                id,
                values(
                    3.0 + f64::from(u32::try_from(n % 20).unwrap()) / 10.0,
                    n % 500,
                    n * 3,
                    n,
                ),
            )
        })
        .collect();
    fetched(&config.data_dir, &with_values);
    let profile = Profile { albums };
    let started = Instant::now();
    let seeds = Seeds::read(&profile, &catalog_support::open(&config.data_dir)).unwrap();
    println!(
        "the seeds' read alone, {} seeds: {:?}",
        seeds.seeds.len(),
        started.elapsed()
    );
    profile::keep(&profile, &config.data_dir).unwrap();
    let kept = Kept::default();
    for time in ["first", "again"] {
        let started = Instant::now();
        let found = rediscover::rediscover(&config, &FakeClock::new(), &kept, &day()).unwrap();
        println!(
            "{time}: {} albums of {} in {:?}",
            found.albums.len(),
            found.albums_considered,
            started.elapsed()
        );
    }
}
