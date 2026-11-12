//! The evaluation on the catalog imported from the sources crate's fixture set (the releases
//! are those `score.rs` describes): the split by a seed, a hidden album ranked among the
//! candidates scored from the kept ones, and the report. The time it takes is measured on
//! ADR 0022's generated set.

#[path = "support/catalog.rs"]
mod catalog_support;
#[path = "support/clock.rs"]
mod clock;
#[path = "support/generated.rs"]
mod generated;
#[path = "../../config/tests/support/setup.rs"]
mod support;

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use catalog::community::Community;
use catalog_support::album;
use clock::FakeClock;
use config::Config;
use features::candidate::Candidate;
use features::evaluate::{self, EvaluateError};
use features::profile::{self, Album, Profile};
use features::score::Connection;

/// The fixture catalog in a fresh data directory named for the test.
fn config(test: &str) -> Config {
    catalog_support::imported_config("evaluate", test, catalog_support::fixture_set)
}

/// Every release of the fixture set tagged on an album of its own, and an untagged album.
fn every_release() -> Profile {
    let mut albums: Vec<Album> = [
        1001, 1002, 1003, 2001, 2002, 2003, 2004, 2005, 2006, 2008, 2011, 2014,
    ]
    .into_iter()
    .map(|id| album("Someone Else", &format!("Album {id}"), Some(id), 3))
    .collect();
    albums.push(album("Hal Unknown", "Untagged", None, 3));
    Profile { albums }
}

#[test]
fn a_hidden_album_reachable_by_one_credit_ranks_first() {
    let config = config("one-credit");
    let catalog = catalog_support::open(&config.data_dir);
    // Ann Example is credited on the band's 1001, and is the artist of 1003.
    let profile = Profile {
        albums: vec![
            album("The Made-Up Band", "First Album", Some(1001), 3),
            album("Ann Example", "Untitled", Some(1003), 3),
        ],
    };
    let hidden = Candidate::read(&catalog, 1003).unwrap().unwrap();
    let ranked = evaluate::rank(&profile, &catalog, &[hidden].into(), None, 1.0).unwrap();
    assert_eq!(ranked.hidden.len(), 1);
    assert_eq!(ranked.hidden[0].candidate, hidden);
    assert_eq!(ranked.hidden[0].rank, Some(1), "{:#?}", ranked.list);
    let first = &ranked.list[0];
    assert!(
        first.reasons.iter().any(|reason| matches!(
            &reason.connection,
            Connection::Credit { artist, .. } if artist.id == 2
        )),
        "{first:#?}"
    );
}

#[test]
fn the_same_seed_gives_the_same_split_and_others_split_otherwise() {
    let config = config("seed");
    let catalog = catalog_support::open(&config.data_dir);
    let profile = every_release();
    let first = evaluate::split(&profile, &catalog, evaluate::SEED).unwrap();
    let again = evaluate::split(&profile, &catalog, evaluate::SEED).unwrap();
    assert_eq!(first.hidden, again.hidden);
    // Master 100 for 1001 and 1002, nine releases without a master, and not the Various
    // Artists compilation 2014, which no list suggests.
    assert_eq!(first.pool, 10);
    let mut splits = HashSet::new();
    for seed in 0..20 {
        let split = evaluate::split(&profile, &catalog, seed).unwrap();
        // A tenth, at least one.
        assert_eq!(split.hidden.len(), 1, "{split:?}");
        assert!(
            !split.hidden.contains(&Candidate::Release(2014)),
            "{split:?}"
        );
        let mut hidden: Vec<Candidate> = split.hidden.into_iter().collect();
        hidden.sort_by_key(|candidate| format!("{candidate:?}"));
        splits.insert(hidden);
    }
    assert!(splits.len() > 1, "{splits:?}");
}

#[test]
fn a_hidden_master_takes_all_its_albums_and_untagged_albums_stay() {
    let config = config("master");
    let catalog = catalog_support::open(&config.data_dir);
    let profile = every_release();
    let ranked = evaluate::rank(
        &profile,
        &catalog,
        &[Candidate::Master(100)].into(),
        None,
        1.0,
    )
    .unwrap();
    assert_eq!(ranked.hidden[0].albums, 2);
    let kept: Vec<Option<u64>> = ranked
        .kept
        .albums
        .iter()
        .map(|album| album.discogs_release_id)
        .collect();
    assert!(
        !kept.contains(&Some(1001)) && !kept.contains(&Some(1002)),
        "{kept:?}"
    );
    assert!(
        kept.contains(&None) && kept.contains(&Some(2001)),
        "{kept:?}"
    );
    assert_eq!(ranked.kept.albums.len(), profile.albums.len() - 2);
}

#[test]
fn a_hidden_album_is_neither_a_seed_nor_excluded_and_a_kept_one_still_excludes() {
    let config = config("excluded");
    let catalog = catalog_support::open(&config.data_dir);
    // DJ Ann's 2008 is an alias's album of Ann Example's 1003; the band's master 100
    // connects through Ann Example's credit, and 1002 is a pressing of it.
    let profile = Profile {
        albums: vec![
            album("Ann Example", "Untitled", Some(1003), 3),
            album("The Made-Up Band", "First Album", Some(1002), 3),
            album("DJ Ann", "DJ Album", Some(2008), 3),
        ],
    };
    let ranked = evaluate::rank(
        &profile,
        &catalog,
        &[Candidate::Release(2008)].into(),
        None,
        1.0,
    )
    .unwrap();
    assert_eq!(ranked.seeds, 2);
    assert!(ranked.hidden[0].rank.is_some(), "{:#?}", ranked.list);
    assert!(!ranked.hidden[0].named);
    let listed: Vec<Candidate> = ranked.list.iter().map(|s| s.candidate).collect();
    assert!(!listed.contains(&Candidate::Master(100)), "{listed:?}");
}

#[test]
fn a_hidden_album_a_kept_album_names_is_left_out() {
    let config = config("named");
    let catalog = catalog_support::open(&config.data_dir);
    let profile = Profile {
        albums: vec![
            album("Ann Example", "Untitled", Some(1003), 3),
            album("DJ Ann", "DJ Album", Some(2008), 3),
            album("DJ Ann", "DJ Album", None, 3),
        ],
    };
    let ranked = evaluate::rank(
        &profile,
        &catalog,
        &[Candidate::Release(2008)].into(),
        None,
        1.0,
    )
    .unwrap();
    assert!(ranked.hidden[0].named, "{:?}", ranked.hidden);
    assert_eq!(ranked.hidden[0].rank, None);
}

#[test]
fn a_hidden_album_is_ranked_by_the_catalog_score_and_with_the_community_values() {
    let config = config("community");
    let catalog = catalog_support::open(&config.data_dir);
    let seeds = vec![
        album("Cora Seed", "Seed", Some(2001), 3),
        album("Ann Example", "Untitled", Some(1003), 3),
    ];
    let catalog_list = evaluate::rank(
        &Profile {
            albums: seeds.clone(),
        },
        &catalog,
        &HashSet::new(),
        None,
        1.0,
    )
    .unwrap()
    .list;
    // The catalog's last release, hidden from a library that has it, is rated well and much
    // owned; every other candidate is rated low.
    let last = catalog_list
        .iter()
        .rev()
        .find_map(|scored| match scored.candidate {
            Candidate::Release(id) => Some(id),
            Candidate::Master(_) => None,
        })
        .unwrap();
    let fetched: Vec<_> = catalog_list
        .iter()
        .map(|scored| {
            let representative = match scored.candidate {
                Candidate::Release(id) => id,
                master @ Candidate::Master(_) => master
                    .entry(&catalog)
                    .unwrap()
                    .unwrap()
                    .main_release
                    .unwrap(),
            };
            let values = if representative == last {
                catalog_support::values(4.8, 400, 5000, 6000)
            } else {
                catalog_support::values(3.2, 40, 20, 2)
            };
            (representative, values)
        })
        .collect();
    catalog_support::fetched(&config.data_dir, &fetched);
    let mut albums = seeds;
    albums.push(album("Someone Else", "Hidden", Some(last), 3));
    let profile = Profile { albums };
    let hidden = [Candidate::Release(last)].into();
    let db = Community::open(&config.data_dir).unwrap();
    let ranked = evaluate::rank(&profile, &catalog, &hidden, Some(&db), 1.0).unwrap();
    let found = &ranked.hidden[0];
    assert!(found.rank.unwrap() > 1, "{ranked:#?}");
    assert_eq!(found.weighed_rank, Some(1), "{ranked:#?}");
    assert!(found.has_values);
    assert_eq!(ranked.with_values, ranked.list.len());
    // At weight 0 the two lists are one.
    let ranked = evaluate::rank(&profile, &catalog, &hidden, Some(&db), 0.0).unwrap();
    assert_eq!(ranked.hidden[0].weighed_rank, ranked.hidden[0].rank);
    let listed: Vec<Candidate> = ranked.list.iter().map(|s| s.candidate).collect();
    assert_eq!(ranked.weighed, listed);
}

/// Every file in `dir`, with its size and modification time.
fn listing(dir: &Path) -> Vec<(String, u64, std::time::SystemTime)> {
    let mut files: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let metadata = entry.metadata().unwrap();
            (
                entry.file_name().to_string_lossy().into_owned(),
                metadata.len(),
                metadata.modified().unwrap(),
            )
        })
        .collect();
    files.sort();
    files
}

#[test]
fn run_reports_the_split_and_writes_nothing() {
    let config = config("run");
    profile::keep(&every_release(), &config.data_dir).unwrap();
    let before = listing(&config.data_dir);
    let report = evaluate::run(&config, &FakeClock::new(), evaluate::SEED).unwrap();
    assert_eq!(listing(&config.data_dir), before);
    let text = report.to_string();
    assert!(
        text.starts_with(
            "the split of seed 1: 1 of the tagged albums' 10 candidates hidden, 1 album\n"
        ),
        "{text}"
    );
    // Every other release of the fixture set is a seed, so the hidden one is all that
    // connects.
    assert!(
        text.contains("\nscored from 11 kept seeds: 1 candidates\n"),
        "{text}"
    );
    assert!(
        text.contains(
            "\nfound: 1 of 1 in the top 100, 1 in the top 500, 1 anywhere, median rank 1\n"
        ),
        "{text}"
    );
    // No community database, so the list with the community values is the same.
    assert!(
        text.contains(
            "\ncommunity values at weight 1: 0 of the 1 hidden, 0 of the 1 candidates\n\
             found with them: 1 of 1 in the top 100, 1 in the top 500, 1 anywhere, median \
             rank 1\n"
        ),
        "{text}"
    );
    assert!(text.ends_with("\ntook 0.0 s"), "{text}");
    // The same split again.
    let again = evaluate::run(&config, &FakeClock::new(), evaluate::SEED).unwrap();
    assert_eq!(again.to_string(), text);
}

#[test]
fn run_refuses_without_a_catalog_a_profile_or_a_tagged_album() {
    let dir = support::fresh_dir("evaluate", "no-catalog");
    let (path, _) = support::write_config(&dir);
    let bare = config::load(&path).unwrap();
    let error = evaluate::run(&bare, &FakeClock::new(), evaluate::SEED).unwrap_err();
    assert!(matches!(error, EvaluateError::NoCatalog), "{error:?}");
    assert!(error.to_string().contains("dumps import"), "{error}");

    let config = config("refused");
    let error = evaluate::run(&config, &FakeClock::new(), evaluate::SEED).unwrap_err();
    assert!(matches!(error, EvaluateError::NoProfile), "{error:?}");
    assert!(error.to_string().contains("profile load"), "{error}");

    let untagged = Profile {
        albums: vec![
            album("Hal Unknown", "Untagged", None, 3),
            album("Hal Unknown", "Not In The Catalog", Some(9998), 3),
        ],
    };
    profile::keep(&untagged, &config.data_dir).unwrap();
    let error = evaluate::run(&config, &FakeClock::new(), evaluate::SEED).unwrap_err();
    assert!(matches!(error, EvaluateError::NoTagged), "{error:?}");
    assert!(error.to_string().contains("tagged"), "{error}");
}

/// The time the evaluation takes on 1M generated releases with 4,000 tagged albums spread
/// over them, as in `score.rs`'s budget. Run with
/// `cargo test --release -p features --test evaluate -- --ignored budget --nocapture`.
#[test]
#[ignore = "a benchmark: generates and imports 1M releases"]
fn budget_of_the_evaluation_on_1m_generated_releases() {
    const RELEASES: u64 = 1_000_000;
    const SEEDS: u64 = 4000;
    let config = catalog_support::imported_config("evaluate", "budget", |dumps| {
        generated::write_set(dumps, RELEASES);
    });
    // The generator's release `id` is by artist `id % (RELEASES / 2) + 1`.
    let albums: Vec<Album> = (0..SEEDS)
        .map(|n| {
            let id = n * (RELEASES / SEEDS) + 1;
            Album {
                plays: n % 7 * 20,
                ..album(
                    &format!("Artist {}", id % (RELEASES / 2) + 1),
                    &format!("Generated Release Number {id}"),
                    Some(id),
                    3,
                )
            }
        })
        .collect();
    profile::keep(&Profile { albums }, &config.data_dir).unwrap();
    let report = evaluate::run(&config, &features::clock::SystemClock, evaluate::SEED).unwrap();
    println!("{report}");
}
