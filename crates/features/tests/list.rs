//! The lists on the catalog imported from the sources crate's fixture set (the seeds are
//! those of `score.rs`): the whole library's read from the stored list, a seed album's and a
//! seed artist's scored when asked, the missing albums of the library's artists, and the
//! artists rolled up from the albums. The cap is tested on its own in `list.rs`, since the
//! fixture has no artist or label with more than 3 candidates. The budget of a seed list is
//! measured on ADR 0022's generated set.

#[path = "support/catalog.rs"]
mod catalog_support;
#[path = "support/clock.rs"]
mod clock;
#[path = "support/generated.rs"]
mod generated;
#[path = "../../config/tests/support/setup.rs"]
mod support;

use std::collections::HashSet;

use catalog::discogs::{Catalog, Reading};
use clock::FakeClock;
use config::Config;
use features::candidate::Candidate;
use features::connection::Connections;
use features::library::Library;
use features::list::{self, Ask, ListError, Lists, Of};
use features::profile::{self, Album, Profile};
use features::rescore;
use features::score::{self, Scope};
use features::seed::Seeds;

/// The fixture catalog in a fresh data directory named for the test.
fn config(test: &str) -> Config {
    catalog_support::imported_config("list", test, catalog_support::fixture_set)
}

/// A library album by `artist`, titled `title`, tagged with `release_id`.
fn album(artist: &str, title: &str, release_id: Option<u64>) -> Album {
    Album {
        artist: artist.to_string(),
        title: title.to_string(),
        track_count: 1,
        durations: vec![180],
        catalog_numbers: Vec::new(),
        barcodes: Vec::new(),
        discogs_release_id: release_id,
        plays: 3,
        loved: 0,
        rating: None,
    }
}

/// Cora Seed's 2001 and Ann Example's 1003, and `more`, kept and scored as `profile load`
/// does.
fn library(config: &Config, more: Vec<Album>) {
    let mut albums = vec![
        album("Cora Seed", "Seed Album", Some(2001)),
        album("Ann Example", "Untitled", Some(1003)),
    ];
    albums.extend(more);
    profile::keep(&Profile { albums }, &config.data_dir).unwrap();
    rescore::run(config, &FakeClock::new(), &mut |_| {}).unwrap();
}

fn ask(of: Of, missing: bool) -> Ask {
    Ask {
        of,
        missing,
        limit: list::LIMIT,
    }
}

fn lists(config: &Config, of: Of, missing: bool) -> Lists {
    list::lists(config, &ask(of, missing)).unwrap()
}

fn candidates(lists: &Lists) -> HashSet<Candidate> {
    lists
        .albums
        .iter()
        .map(|album| album.scored.candidate)
        .collect()
}

#[test]
fn the_whole_library_s_list_is_the_stored_one_with_its_names() {
    let config = config("library");
    library(&config, Vec::new());
    let lists = lists(&config, Of::Library, false);
    let stored = rescore::top(&config.data_dir, u64::MAX).unwrap().unwrap();
    let in_order: Vec<Candidate> = lists.albums.iter().map(|a| a.scored.candidate).collect();
    let expected: Vec<Candidate> = stored
        .candidates
        .iter()
        .map(|listed| listed.scored.candidate)
        .collect();
    assert_eq!(in_order, expected);
    let text = lists.to_string();
    assert!(
        text.starts_with("the whole library: the list computed from the set of 20000101"),
        "{text}"
    );
    // Gus Other's album produced by Dee Producer, as the row and its reasons.
    assert!(
        text.contains("Gus Other – Produced Album (2001), release 2003\n"),
        "{text}"
    );
    assert!(
        text.contains("Producer: Dee Producer, Producer on 1 album you have"),
        "{text}"
    );
    assert!(text.contains("\nartists\n"), "{text}");
}

#[test]
fn without_a_stored_list_the_whole_library_is_refused_with_how_to_make_one() {
    let config = config("no-list");
    profile::keep(
        &Profile {
            albums: vec![album("Cora Seed", "Seed Album", Some(2001))],
        },
        &config.data_dir,
    )
    .unwrap();
    let error = list::lists(&config, &ask(Of::Library, false)).unwrap_err();
    assert!(matches!(error, ListError::NoList), "{error:?}");
    assert!(error.to_string().contains("profile load"), "{error}");
}

#[test]
fn a_list_computed_from_another_profile_is_refused_until_it_is_rebuilt() {
    let config = config("stale");
    library(&config, Vec::new());
    // A new profile is kept; the rebuild it asks for hasn't finished.
    let mut albums = vec![
        album("Cora Seed", "Seed Album", Some(2001)),
        album("Ann Example", "Untitled", Some(1003)),
    ];
    albums.push(album("New Band", "New Album", None));
    profile::keep(&Profile { albums }, &config.data_dir).unwrap();
    let error = list::lists(&config, &ask(Of::Library, false)).unwrap_err();
    assert!(matches!(error, ListError::Stale { .. }), "{error:?}");
    // No rebuild is running, so waiting would not help: it says what rebuilds now.
    let words = error.to_string();
    assert!(words.contains("out of date"), "{words}");
    assert!(words.contains("no rebuild is running"), "{words}");
    assert!(words.contains("profile load"), "{words}");
    // With one running, the words say so, and to ask again when it ends.
    let rebuilding = catalog::score::lock(&config.data_dir, || {}).unwrap();
    let error = list::lists(&config, &ask(Of::Library, false)).unwrap_err();
    let words = error.to_string();
    assert!(words.contains("a rebuild is running"), "{words}");
    assert!(words.contains("try again"), "{words}");
    drop(rebuilding);
    // A seed list is scored when asked, so it doesn't read the stored one.
    lists(&config, Of::Album(2001), false);
    rescore::run(&config, &FakeClock::new(), &mut |_| {}).unwrap();
    lists(&config, Of::Library, false);
}

#[test]
fn a_seed_album_s_list_comes_from_its_connections_only() {
    let config = config("album");
    library(&config, Vec::new());
    let whole = candidates(&lists(&config, Of::Library, false));
    // DJ Ann's 2008 and the band's master 100 connect through Ann Example's 1003.
    assert!(whole.contains(&Candidate::Release(2008)), "{whole:?}");
    let cora = lists(&config, Of::Album(2001), false);
    let found = candidates(&cora);
    assert!(found.contains(&Candidate::Release(2003)), "{found:?}");
    assert!(found.contains(&Candidate::Release(2002)), "{found:?}");
    assert!(!found.contains(&Candidate::Release(2008)), "{found:?}");
    assert!(!found.contains(&Candidate::Master(100)), "{found:?}");
    assert!(
        cora.to_string()
            .starts_with("the seed album 2001: Cora Seed – Seed Album\n"),
        "{cora}"
    );
    // The seed's scores are those of the whole profile's seeds scored for it alone.
    let Reading::Open(catalog) = Catalog::open_read_only(&config.data_dir).unwrap() else {
        panic!("no catalog");
    };
    let profile = profile::kept(&config.data_dir).unwrap().unwrap();
    let seeds = Seeds::read(&profile, &catalog).unwrap();
    let connections = Connections::read(&catalog, &seeds).unwrap();
    let library = Library::new(&profile, &catalog).unwrap();
    let expected = score::score(
        &seeds,
        &connections,
        &library,
        &Scope::Releases([2001].into()),
    );
    assert_eq!(cora.albums.len(), expected.len());
    for (album, scored) in cora.albums.iter().zip(&expected) {
        assert_eq!(album.scored.candidate, scored.candidate);
        assert!(
            (album.scored.score - scored.score).abs() < 1e-12,
            "{album:?}"
        );
        assert_eq!(album.scored.reasons, scored.reasons);
    }
}

#[test]
fn a_seed_artist_s_list_is_its_seeds_by_the_folded_name() {
    let config = config("artist");
    library(&config, Vec::new());
    let by_name = lists(&config, Of::Artist("CORA  seed".to_string()), false);
    assert_eq!(
        candidates(&by_name),
        candidates(&lists(&config, Of::Album(2001), false))
    );
    assert!(
        by_name
            .to_string()
            .starts_with("the 1 seed album by CORA  seed\n"),
        "{by_name}"
    );
}

#[test]
fn an_album_or_artist_not_among_the_seeds_is_refused_naming_it() {
    let config = config("refused");
    library(
        &config,
        vec![
            album("Gus Other", "Untagged", None),
            album("Hal Unknown", "Not In The Catalog", Some(9998)),
        ],
    );
    let refused = |of| {
        list::lists(&config, &ask(of, false))
            .unwrap_err()
            .to_string()
    };
    let error = refused(Of::Album(9999));
    assert!(
        error.contains("9999") && error.contains("profile"),
        "{error}"
    );
    let error = refused(Of::Album(9998));
    assert!(
        error.contains("9998") && error.contains("catalog"),
        "{error}"
    );
    let error = refused(Of::Artist("Nobody".to_string()));
    assert!(error.contains("`Nobody`"), "{error}");
    // In the library, but on no seed.
    let error = refused(Of::Artist("Gus Other".to_string()));
    assert!(
        error.contains("`Gus Other`") && error.contains("seed"),
        "{error}"
    );
}

#[test]
fn missing_keeps_only_albums_by_the_library_s_artists() {
    let config = config("missing");
    library(&config, Vec::new());
    let all = candidates(&lists(&config, Of::Library, false));
    assert!(all.contains(&Candidate::Release(2003)), "{all:?}");
    let missing = lists(&config, Of::Library, true);
    // Cora Seed's 2002 and 2011; Gus Other's 2003 and DJ Ann's 2008 are left out.
    assert_eq!(
        candidates(&missing),
        [Candidate::Release(2002), Candidate::Release(2011)].into()
    );
    assert!(missing.artists.is_none());
    // With a scope, the scope's artists: Ann Example has no other album.
    let ann = lists(&config, Of::Album(1003), true);
    assert!(ann.albums.is_empty(), "{:?}", candidates(&ann));
    let cora = lists(&config, Of::Artist("Cora Seed".to_string()), true);
    assert_eq!(candidates(&cora), candidates(&missing));
}

#[test]
fn the_artists_roll_up_their_albums_and_leave_out_the_library_s() {
    let config = config("artists");
    library(&config, Vec::new());
    let lists = lists(&config, Of::Library, false);
    let artists = lists.artists.as_ref().unwrap();
    let ids: Vec<u64> = artists.iter().map(|artist| artist.id).collect();
    // Gus Other (23) and DJ Ann (4); Cora Seed (20) and Ann Example (2) are the library's.
    assert!(ids.contains(&23) && ids.contains(&4), "{ids:?}");
    assert!(!ids.contains(&20) && !ids.contains(&2), "{ids:?}");
    assert!((artists[0].score - 1.0).abs() < 1e-12, "{artists:?}");
    // Gus Other: 2003, 2004 and 2005, the best whole, the next at a quarter and a tenth.
    let album_score = |id| {
        lists
            .albums
            .iter()
            .find(|album| album.scored.candidate == Candidate::Release(id))
            .unwrap()
            .scored
            .score
    };
    let gus = artists.iter().find(|artist| artist.id == 23).unwrap();
    let ann = artists.iter().find(|artist| artist.id == 4).unwrap();
    let mut scores = [album_score(2003), album_score(2004), album_score(2005)];
    scores.sort_by(|a, b| b.total_cmp(a));
    let rolled = scores[0] + 0.25 * scores[1] + 0.1 * scores[2];
    // DJ Ann has 2008 alone.
    assert!(
        (gus.score / ann.score - rolled / album_score(2008)).abs() < 1e-9,
        "{gus:?} {ann:?} {scores:?}"
    );
    assert_eq!(gus.albums, 3);
}

#[test]
fn the_whole_library_s_artists_are_read_from_the_stored_list() {
    use catalog::score::{ArtistRow, Row, Scores, Writer};
    let config = config("artists-stored");
    library(&config, Vec::new());
    // The stored list written again with one artist the candidates don't give.
    let Reading::Open(scores) = Scores::open_read_only(&config.data_dir).unwrap() else {
        panic!("no scores");
    };
    let computed = scores.computed().unwrap();
    let rows: Vec<Row> = scores.top(u64::MAX).unwrap();
    drop(scores);
    let mut writer = Writer::create(&config.data_dir).unwrap();
    for row in &rows {
        writer.add(row).unwrap();
    }
    writer
        .add_artist(&ArtistRow {
            id: 999,
            name: Some("Stored Only".to_string()),
            score: 1.0,
            albums: 1,
            best_rank: 1,
        })
        .unwrap();
    writer
        .finish(&computed.inputs, computed.seeds, computed.finished_at)
        .unwrap();
    let lists = lists(&config, Of::Library, false);
    let artists = lists.artists.as_ref().unwrap();
    assert_eq!(artists.len(), 1, "{artists:?}");
    let text = lists.to_string();
    assert!(
        text.contains("  1. 1.00  Stored Only, artist 999, from 1 album\n        best: "),
        "{text}"
    );
}

#[test]
fn a_master_the_catalog_has_no_row_for_is_listed_with_its_first_release_s_title() {
    let config = config("no-master-row");
    // The masters dump can lack a master its releases name: here the band's, 100.
    rusqlite::Connection::open(config.data_dir.join("catalog.sqlite"))
        .unwrap()
        .execute("DELETE FROM master WHERE id = 100", [])
        .unwrap();
    library(&config, Vec::new());
    let lists = lists(&config, Of::Library, false);
    let text = lists.to_string();
    assert!(
        text.contains("The Made-Up Band – First Album (1999), master 100\n"),
        "{text}"
    );
    assert!(!text.contains("no title in the catalog"), "{text}");
}

#[test]
fn an_untagged_library_album_leaves_its_artist_out_of_the_artists_by_name() {
    let config = config("artists-by-name");
    library(&config, vec![album("Gus Other", "Untagged", None)]);
    let lists = list::lists(&config, &ask(Of::Library, false)).unwrap();
    let ids: Vec<u64> = lists.artists.unwrap().iter().map(|a| a.id).collect();
    assert!(!ids.contains(&23), "{ids:?}");
}

/// The time a list takes on 1M generated releases with 4,000 seeds spread over them, as in
/// `score.rs`'s budget: the whole library's, read from the stored list, and a seed album's
/// and a seed artist's, each scored from one pass over the catalog. Run with
/// `cargo test --release -p features --test list -- --ignored budget --nocapture`.
#[test]
#[ignore = "a benchmark: generates and imports 1M releases"]
fn budget_of_the_lists_on_1m_generated_releases() {
    use std::time::Instant;
    const RELEASES: u64 = 1_000_000;
    const SEEDS: u64 = 4000;
    let config = catalog_support::imported_config("list", "budget", |dumps| {
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
                )
            }
        })
        .collect();
    let started = Instant::now();
    profile::keep(&Profile { albums }, &config.data_dir).unwrap();
    rescore::run(&config, &FakeClock::new(), &mut |_| {}).unwrap();
    println!(
        "the whole library scored and stored in {:?}",
        started.elapsed()
    );
    for of in [
        Of::Library,
        Of::Album(1),
        Of::Artist("Artist 2".to_string()),
    ] {
        let started = Instant::now();
        let lists = lists(&config, of.clone(), false);
        println!(
            "{of:?}: {} albums and {} artists in {:?}",
            lists.albums.len(),
            lists.artists.as_ref().map_or(0, Vec::len),
            started.elapsed()
        );
    }
}
