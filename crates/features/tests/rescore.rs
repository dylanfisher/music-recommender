//! The scores' rebuild on the catalog imported from the sources crate's fixture set (the
//! seeds are those of `score.rs`): the whole library's list written to `scores.sqlite` and
//! read back with its reasons, rebuilt only when the catalog or what the score reads from
//! the profile changed, and the old list kept when a rebuild stops before its rename.

#[path = "support/catalog.rs"]
mod catalog_support;
#[path = "support/clock.rs"]
mod clock;
#[path = "support/generated.rs"]
mod generated;
#[path = "../../config/tests/support/setup.rs"]
mod support;

use std::fs;
use std::path::Path;
use std::time::Duration;

use catalog::discogs::{Catalog, Reading};
use catalog::score::Writer;
use clock::FakeClock;
use config::Config;
use features::candidate::Candidate;
use features::connection::Connections;
use features::library::Library;
use features::profile::{self, Album, Profile};
use features::rescore::{self, Summary};
use features::score;
use features::seed::Seeds;
use features::{clock::Clock, import};

/// The fixture catalog in a fresh data directory named for the test.
fn config(test: &str) -> Config {
    catalog_support::imported_config("rescore", test, catalog_support::fixture_set)
}

/// A library album tagged with `release_id`, played `plays` times.
fn album(release_id: u64, plays: u64) -> Album {
    Album {
        artist: "Someone Else".to_string(),
        title: format!("Album {release_id}"),
        track_count: 1,
        durations: vec![180],
        catalog_numbers: Vec::new(),
        barcodes: Vec::new(),
        discogs_release_id: Some(release_id),
        plays,
        loved: 0,
        rating: None,
    }
}

/// Keeps a profile of `albums` in the config's data directory, as `POST /v1/profile` does.
fn keep(config: &Config, albums: Vec<Album>) {
    profile::keep(&Profile { albums }, &config.data_dir).unwrap();
}

fn run(config: &Config, clock: &FakeClock) -> Summary {
    rescore::run(config, clock, &mut |_| {}).unwrap()
}

fn open_catalog(data_dir: &Path) -> Catalog {
    let Reading::Open(catalog) = Catalog::open_read_only(data_dir).unwrap() else {
        panic!("no catalog");
    };
    catalog
}

#[test]
fn a_rebuild_writes_the_whole_library_s_list_with_its_reasons() {
    let config = config("written");
    let clock = FakeClock::new();
    keep(&config, vec![album(2001, 3), album(1003, 0)]);
    let Summary::Built(computed) = run(&config, &clock) else {
        panic!("not built");
    };
    let catalog = open_catalog(&config.data_dir);
    let profile = profile::kept(&config.data_dir).unwrap().unwrap();
    let seeds = Seeds::read(&profile, &catalog).unwrap();
    let connections = Connections::read(&catalog, &seeds).unwrap();
    let library = Library::new(&profile, &catalog).unwrap();
    let expected = score::score(&seeds, &connections, &library);
    assert!(expected.len() > 3, "{expected:?}");
    assert_eq!(computed.seeds, 2);
    assert_eq!(computed.candidates, expected.len() as u64);
    assert_eq!(computed.finished_at, clock::START);
    assert_eq!(computed.inputs.catalog_set, "20000101");

    let list = rescore::top(&config.data_dir, u64::MAX).unwrap().unwrap();
    assert_eq!(list.computed, computed);
    assert_eq!(list.candidates.len(), expected.len());
    for (listed, scored) in list.candidates.iter().zip(&expected) {
        assert_eq!(listed.scored.candidate, scored.candidate);
        assert!(
            (listed.scored.score - scored.score).abs() < f64::EPSILON,
            "{listed:?}"
        );
        assert_eq!(listed.scored.reasons, scored.reasons);
        assert_eq!(
            listed.representative,
            scored.candidate.representative(&catalog).unwrap()
        );
    }
    let kinds: Vec<_> = list
        .candidates
        .iter()
        .flat_map(|listed| &listed.scored.reasons)
        .map(|reason| std::mem::discriminant(&reason.connection))
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    // Every kind of connection the fixture has went through the file and back.
    assert_eq!(kinds.len(), 5, "{list:#?}");
    let top = rescore::top(&config.data_dir, 2).unwrap().unwrap();
    assert_eq!(top.candidates.len(), 2);
    assert_eq!(top.candidates[0].scored.candidate, expected[0].candidate);
}

#[test]
fn the_stored_artists_are_a_full_roll_up_of_the_stored_albums() {
    let config = config("artists");
    keep(&config, vec![album(2001, 3), album(1003, 0)]);
    run(&config, &FakeClock::new());
    let catalog = open_catalog(&config.data_dir);
    let list = rescore::top(&config.data_dir, u64::MAX).unwrap().unwrap();
    let stored = rescore::top_artists(&config.data_dir, u64::MAX)
        .unwrap()
        .unwrap();
    assert_eq!(stored.computed, list.computed);
    // The artists of the tagged releases, and Various, aren't rolled up.
    let mut left_out: Vec<u64> = vec![194];
    for release in [2001, 1003] {
        left_out.extend(catalog.links(release).unwrap().unwrap().artists);
    }
    // Every candidate's artists, the best three of each artist's candidates in rank order.
    let mut expected: Vec<(u64, f64, usize, usize)> = Vec::new();
    for (place, listed) in list.candidates.iter().enumerate() {
        let entry = listed.scored.candidate.entry(&catalog).unwrap().unwrap();
        for (id, _) in entry.artists {
            if left_out.contains(&id) {
                continue;
            }
            let known = expected.iter().position(|artist| artist.0 == id);
            let at = known.unwrap_or_else(|| {
                expected.push((id, 0.0, 0, place));
                expected.len() - 1
            });
            let artist = &mut expected[at];
            if let Some(share) = [1.0, 0.25, 0.1].get(artist.2) {
                artist.1 += share * listed.scored.score;
                artist.2 += 1;
            }
        }
    }
    expected.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let best = expected[0].1;
    assert!(expected.len() > 2, "{expected:?}");
    assert_eq!(stored.artists.len(), expected.len());
    for (artist, (id, score, albums, place)) in stored.artists.iter().zip(&expected) {
        assert_eq!(artist.id, *id);
        assert!((artist.score - score / best).abs() < 1e-12, "{artist:?}");
        assert_eq!(artist.albums, *albums);
        assert_eq!(
            artist.best.scored.candidate,
            list.candidates[*place].scored.candidate
        );
        assert_eq!(
            artist.best.scored.reasons,
            list.candidates[*place].scored.reasons
        );
    }
    let first = rescore::top_artists(&config.data_dir, 1).unwrap().unwrap();
    assert_eq!(first.artists.len(), 1);
    assert_eq!(first.artists[0].id, expected[0].0);
}

#[test]
fn the_same_seeds_rebuild_nothing_and_a_changed_seed_rebuilds() {
    let config = config("same-seeds");
    let clock = FakeClock::new();
    keep(&config, vec![album(2001, 3), album(1003, 0)]);
    assert!(matches!(run(&config, &clock), Summary::Built(_)));
    clock.sleep(Duration::from_mins(5));
    // The same profile kept again, its albums in another order.
    keep(&config, vec![album(1003, 0), album(2001, 3)]);
    let Summary::Current(computed) = run(&config, &clock) else {
        panic!("rebuilt");
    };
    assert_eq!(computed.finished_at, clock::START);
    // A play more changes the seed's weight.
    keep(&config, vec![album(1003, 0), album(2001, 4)]);
    let Summary::Built(computed) = run(&config, &clock) else {
        panic!("not rebuilt");
    };
    assert_eq!(computed.finished_at, clock.seconds());
    // So does a seed gone.
    keep(&config, vec![album(2001, 4)]);
    let Summary::Built(computed) = run(&config, &clock) else {
        panic!("not rebuilt");
    };
    assert_eq!(computed.seeds, 1);
}

#[test]
fn a_list_from_another_scoring_version_is_rebuilt() {
    let config = config("scoring-version");
    let clock = FakeClock::new();
    keep(&config, vec![album(2001, 3), album(1003, 0)]);
    assert!(matches!(run(&config, &clock), Summary::Built(_)));
    // The list as a build with other weights or rules left it.
    rusqlite::Connection::open(config.data_dir.join("scores.sqlite"))
        .unwrap()
        .execute(
            "UPDATE computed SET scoring_version = scoring_version + 1",
            [],
        )
        .unwrap();
    let Summary::Built(computed) = run(&config, &clock) else {
        panic!("not rebuilt");
    };
    assert_eq!(computed.inputs.scoring_version, score::VERSION);
    assert!(matches!(run(&config, &clock), Summary::Current(_)));
}

#[test]
fn a_stored_list_that_can_t_be_read_is_rebuilt_over() {
    let config = config("unreadable");
    let clock = FakeClock::new();
    keep(&config, vec![album(2001, 3), album(1003, 0)]);
    assert!(matches!(run(&config, &clock), Summary::Built(_)));
    // Of the right shape, but without the `computed` row every rebuild writes.
    rusqlite::Connection::open(config.data_dir.join("scores.sqlite"))
        .unwrap()
        .execute("DELETE FROM computed", [])
        .unwrap();
    let mut log = Vec::new();
    let summary = rescore::run(&config, &clock, &mut |line| log.push(line.to_string())).unwrap();
    assert!(matches!(summary, Summary::Built(_)), "{summary:?}");
    assert!(
        log.iter().any(|line| line.contains("can't be read")),
        "{log:?}"
    );
    // The list is whole again, and the next rebuild finds it current.
    assert!(rescore::top(&config.data_dir, 1).unwrap().is_some());
    assert!(matches!(run(&config, &clock), Summary::Current(_)));
}

#[test]
fn a_profile_with_no_seeds_writes_an_empty_list_without_scanning_the_catalog() {
    let config = config("no-seeds");
    // A release row the scan can't read, so a rebuild that scans fails.
    rusqlite::Connection::open(config.data_dir.join("catalog.sqlite"))
        .unwrap()
        .execute(
            "UPDATE release SET styles = x'0100000002' WHERE id = 2001",
            [],
        )
        .unwrap();
    // No album of the library is tagged with a release.
    let untagged = Album {
        discogs_release_id: None,
        ..album(2001, 3)
    };
    keep(&config, vec![untagged]);
    let Summary::Built(computed) = run(&config, &FakeClock::new()) else {
        panic!("not built");
    };
    assert_eq!((computed.seeds, computed.candidates), (0, 0));
    let list = rescore::top(&config.data_dir, u64::MAX).unwrap().unwrap();
    assert!(list.candidates.is_empty());
}

#[test]
fn a_new_catalog_rebuilds() {
    let config = config("new-catalog");
    let clock = FakeClock::new();
    keep(&config, vec![album(2001, 3)]);
    assert!(matches!(run(&config, &clock), Summary::Built(_)));
    // The same set imported again a day later is a catalog swapped in at another time.
    clock.sleep(Duration::from_hours(24));
    fs::remove_file(config.data_dir.join("catalog.sqlite")).unwrap();
    import::run(&config, &clock, import::BATCH, &mut |_| {}).unwrap();
    let Summary::Built(computed) = run(&config, &clock) else {
        panic!("not rebuilt");
    };
    assert_eq!(computed.inputs.catalog_swapped_at, clock.seconds());
    assert!(matches!(run(&config, &clock), Summary::Current(_)));
}

#[test]
fn a_catalog_with_a_credit_whose_role_has_no_name_is_an_error_not_a_panic() {
    let config = config("role-without-name");
    keep(&config, vec![album(2001, 3), album(1003, 0)]);
    // A build writes every role a credit uses, so this catalog is damaged.
    rusqlite::Connection::open(config.data_dir.join("catalog.sqlite"))
        .unwrap()
        .execute("DELETE FROM role", [])
        .unwrap();
    let error = rescore::run(&config, &FakeClock::new(), &mut |_| {}).unwrap_err();
    assert!(error.to_string().contains("has no name"), "{error}");
    // The failed rebuild wrote no list.
    assert!(rescore::top(&config.data_dir, 1).unwrap().is_none());
}

#[test]
fn without_a_catalog_or_a_profile_nothing_is_computed() {
    let dir = support::fresh_dir("rescore", "nothing");
    let (path, data_dir) = support::write_config(&dir);
    let bare = config::load(&path).unwrap();
    keep(&bare, vec![album(2001, 3)]);
    let summary = run(&bare, &FakeClock::new());
    assert!(matches!(summary, Summary::NoCatalog), "{summary:?}");
    assert!(summary.to_string().contains("no catalog"), "{summary}");

    let config = config("no-profile");
    let summary = run(&config, &FakeClock::new());
    assert!(matches!(summary, Summary::NoProfile), "{summary:?}");
    assert!(summary.to_string().contains("no profile"), "{summary}");
    assert!(rescore::top(&config.data_dir, 10).unwrap().is_none());
    assert!(!data_dir.join("scores.sqlite").exists());
}

#[test]
fn a_rebuild_interrupted_leaves_the_old_scores_in_use() {
    let config = config("interrupted");
    let clock = FakeClock::new();
    keep(&config, vec![album(2001, 3), album(1003, 0)]);
    assert!(matches!(run(&config, &clock), Summary::Built(_)));
    let before = rescore::top(&config.data_dir, u64::MAX).unwrap().unwrap();
    // A rebuild that stopped after its first rows, as a failed write or a killed process
    // leaves one.
    let mut writer = Writer::create(&config.data_dir).unwrap();
    writer
        .add(&catalog::score::Row {
            kind: catalog::feedback::Kind::Release,
            id: 1,
            representative: Some(1),
            score: 1.0,
            reasons: Vec::new(),
        })
        .unwrap();
    drop(writer);
    let after = rescore::top(&config.data_dir, u64::MAX).unwrap().unwrap();
    assert_eq!(after.computed, before.computed);
    assert_eq!(after.candidates.len(), before.candidates.len());
    assert!(config.data_dir.join("scores.next.sqlite").exists());

    clock.sleep(Duration::from_mins(1));
    keep(&config, vec![album(2001, 3)]);
    let Summary::Built(computed) = run(&config, &clock) else {
        panic!("not rebuilt");
    };
    assert!(!config.data_dir.join("scores.next.sqlite").exists());
    let list = rescore::top(&config.data_dir, u64::MAX).unwrap().unwrap();
    assert_eq!(list.computed, computed);
    assert!(
        list.candidates
            .iter()
            .all(|listed| listed.scored.candidate != Candidate::Release(1))
    );
}

/// The budget of a list read from `scores.sqlite` (plan, phase 4): under 100 ms. Measured
/// on 1M generated releases with 4,000 seeds spread over them, as the score's budget is
/// (`score.rs`), with the rebuild's whole time beside it; recorded in ADR 0026. Run with
/// `cargo test --release -p features --test rescore -- --ignored budget --nocapture`.
#[test]
#[ignore = "a benchmark: generates and imports 1M releases"]
fn budget_of_the_list_on_1m_generated_releases() {
    use std::time::Instant;
    const RELEASES: u64 = 1_000_000;
    const SEEDS: u64 = 4000;
    let config = catalog_support::imported_config("rescore", "budget", |dumps| {
        generated::write_set(dumps, RELEASES);
    });
    // The generator's release `id` is by artist `id % (RELEASES / 2) + 1`, as in `score.rs`.
    let albums = (0..SEEDS)
        .map(|n| {
            let id = n * (RELEASES / SEEDS) + 1;
            Album {
                artist: format!("Artist {}", id % (RELEASES / 2) + 1),
                title: format!("Generated Release Number {id}"),
                ..album(id, n % 7 * 20)
            }
        })
        .collect();
    keep(&config, albums);
    let clock = FakeClock::new();
    let started = Instant::now();
    let Summary::Built(computed) = run(&config, &clock) else {
        panic!("not built");
    };
    println!(
        "rebuilt in {:?}: {} candidates from {} seeds; scores.sqlite is {} bytes",
        started.elapsed(),
        computed.candidates,
        computed.seeds,
        fs::metadata(config.data_dir.join("scores.sqlite"))
            .unwrap()
            .len()
    );
    let started = Instant::now();
    assert!(matches!(run(&config, &clock), Summary::Current(_)));
    println!("found current in {:?}", started.elapsed());
    for limit in [100, 500, u64::MAX] {
        let started = Instant::now();
        let list = rescore::top(&config.data_dir, limit).unwrap().unwrap();
        println!(
            "read the first {} candidates in {:?}",
            list.candidates.len(),
            started.elapsed()
        );
    }
}
