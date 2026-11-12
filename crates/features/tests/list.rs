//! The lists on the catalog imported from the sources crate's fixture set (the seeds are
//! those of `score.rs`): the whole library's read from the stored list, a seed album's and a
//! seed artist's computed when first asked and kept on disk, the missing albums of the
//! library's artists, the filters, the feedback's marks left out, pages, and the artists
//! rolled up from the albums. The fixture has no artist or label with more than 3
//! candidates, so the cap across pages is tested on a stored list written for it. The
//! budgets are measured on ADR 0022's generated set.

#[path = "support/catalog.rs"]
mod catalog_support;
#[path = "support/clock.rs"]
mod clock;
#[path = "support/generated.rs"]
mod generated;
#[path = "../../config/tests/support/setup.rs"]
mod support;

use std::collections::HashSet;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use catalog::discogs::Reading;
use catalog::feedback::Kind;
use catalog::score::{Named, Row, Scores, Writer};
use catalog_support::album;
use clock::FakeClock;
use config::Config;
use features::candidate::Candidate;
use features::connection::Connections;
use features::feedback::{self, Verdict};
use features::library::Library;
use features::list::{self, Ask, Filter, List, ListError, Lists, Of};
use features::profile::{self, Album, Profile};
use features::rescore;
use features::score;
use features::seed::Seeds;

/// The fixture catalog in a fresh data directory named for the test.
fn config(test: &str) -> Config {
    catalog_support::imported_config("list", test, catalog_support::fixture_set)
}

/// Cora Seed's 2001 and Ann Example's 1003, and `more`, kept and scored as `profile load`
/// does.
fn library(config: &Config, more: Vec<Album>) {
    let mut albums = vec![
        album("Cora Seed", "Seed Album", Some(2001), 3),
        album("Ann Example", "Untitled", Some(1003), 3),
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
    list::lists(config, &FakeClock::new(), &ask(of, missing), &mut |_| {}).unwrap()
}

/// The error `lists` returns for `of`.
fn refused(config: &Config, of: Of) -> ListError {
    list::lists(config, &FakeClock::new(), &ask(of, false), &mut |_| {}).unwrap_err()
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
    let stored = rescore::page(&config.data_dir, 0, u64::MAX)
        .unwrap()
        .unwrap();
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
            albums: vec![album("Cora Seed", "Seed Album", Some(2001), 3)],
        },
        &config.data_dir,
    )
    .unwrap();
    let error = refused(&config, Of::Library);
    assert!(matches!(error, ListError::NoList), "{error:?}");
    assert!(error.to_string().contains("profile load"), "{error}");
}

#[test]
fn a_list_computed_from_another_profile_is_refused_until_it_is_rebuilt() {
    let config = config("stale");
    library(&config, Vec::new());
    // A new profile is kept; the rebuild it asks for hasn't finished.
    let mut albums = vec![
        album("Cora Seed", "Seed Album", Some(2001), 3),
        album("Ann Example", "Untitled", Some(1003), 3),
    ];
    albums.push(album("New Band", "New Album", None, 3));
    profile::keep(&Profile { albums }, &config.data_dir).unwrap();
    let error = refused(&config, Of::Library);
    assert!(matches!(error, ListError::Stale { .. }), "{error:?}");
    // No rebuild is running, so waiting would not help: it says what rebuilds now.
    let words = error.to_string();
    assert!(words.contains("out of date"), "{words}");
    assert!(words.contains("no rebuild is running"), "{words}");
    assert!(words.contains("profile load"), "{words}");
    // With one running, the words say so, and to ask again when it ends.
    let rebuilding = catalog::score::lock(&config.data_dir, || {}).unwrap();
    let error = refused(&config, Of::Library);
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
    let catalog = catalog_support::open(&config.data_dir);
    let profile = profile::kept(&config.data_dir).unwrap().unwrap();
    let library = Library::new(&profile, &catalog).unwrap();
    let cora_albums = Profile {
        albums: profile
            .albums
            .iter()
            .filter(|album| album.discogs_release_id == Some(2001))
            .cloned()
            .collect(),
    };
    let seeds = Seeds::read(&cora_albums, &catalog).unwrap();
    let connections = Connections::read(&catalog, &seeds).unwrap();
    let expected = score::score(&seeds, connections, &library).unwrap();
    assert_eq!(cora.albums.len(), expected.len());
    for (album, (scored, _)) in cora.albums.iter().zip(&expected) {
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
            album("Gus Other", "Untagged", None, 3),
            album("Hal Unknown", "Not In The Catalog", Some(9998), 3),
        ],
    );
    let refused = |of| refused(&config, of).to_string();
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
    use catalog::score::ArtistRow;
    let config = config("artists-stored");
    library(&config, Vec::new());
    // The stored list written again with one artist the candidates don't give.
    let Reading::Open(scores) = Scores::open_read_only(&config.data_dir).unwrap() else {
        panic!("no scores");
    };
    let computed = scores.computed().unwrap();
    let rows: Vec<Row> = scores.page(0, u64::MAX).unwrap();
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
    library(&config, vec![album("Gus Other", "Untagged", None, 3)]);
    let lists = lists(&config, Of::Library, false);
    let ids: Vec<u64> = lists.artists.unwrap().iter().map(|a| a.id).collect();
    assert!(!ids.contains(&23), "{ids:?}");
}

/// The list `of`, opened as the route and `recommend` open it.
fn open(config: &Config, of: &Of) -> List {
    List::open(config, &FakeClock::new(), of, &mut |_| {}).unwrap()
}

/// A page of the albums of `list`, and whether there are more.
fn page(list: &List, filter: &Filter, offset: usize, limit: usize) -> (Vec<rescore::Listed>, bool) {
    let page = list.albums(filter, false, offset, limit).unwrap();
    (page.items, page.more)
}

fn ids(albums: &[rescore::Listed]) -> Vec<Candidate> {
    albums.iter().map(|album| album.scored.candidate).collect()
}

#[test]
fn each_filter_keeps_the_albums_it_asks_for() {
    let config = config("filters");
    library(&config, Vec::new());
    let list = open(&config, &Of::Library);
    let (all, more) = page(&list, &Filter::default(), 0, 100);
    assert!(!more);
    // What each filter should keep, read from the rows of the whole list.
    let keeps = |filter: &Filter, expected: &dyn Fn(&rescore::Listed) -> bool| {
        let (kept, _) = page(&list, filter, 0, 100);
        let wanted: Vec<Candidate> = all
            .iter()
            .filter(|album| expected(album))
            .map(|album| album.scored.candidate)
            .collect();
        assert!(!wanted.is_empty() && wanted.len() < all.len(), "{wanted:?}");
        assert_eq!(ids(&kept), wanted, "{filter:?}");
    };
    keeps(
        &Filter {
            styles: vec!["made-up common".to_string()],
            ..Filter::default()
        },
        &|album| {
            album
                .styles
                .iter()
                .any(|style| style.name.as_deref() == Some("Made-Up Common"))
        },
    );
    keeps(
        &Filter {
            years: Some((2001, 2003)),
            ..Filter::default()
        },
        &|album| album.year.is_some_and(|year| (2001..=2003).contains(&year)),
    );
    // Seed Records, by its Discogs id.
    keeps(
        &Filter {
            label_ids: vec![30],
            ..Filter::default()
        },
        &|album| album.label.as_ref().is_some_and(|label| label.id == 30),
    );
}

/// Writes the stored list again as `rows`, from the inputs it was computed from, so a test
/// shapes a list the fixture can't make.
fn store(config: &Config, rows: &[Row]) {
    let Reading::Open(scores) = Scores::open_read_only(&config.data_dir).unwrap() else {
        panic!("no scores");
    };
    let computed = scores.computed().unwrap();
    drop(scores);
    let mut writer = Writer::create(&config.data_dir).unwrap();
    for row in rows {
        writer.add(row).unwrap();
    }
    writer
        .finish(&computed.inputs, computed.seeds, computed.finished_at)
        .unwrap();
}

/// A stored row for release `id` by the artist `artist`.
fn by(id: u64, artist: u64) -> Row {
    Row {
        kind: Kind::Release,
        id,
        representative: Some(id),
        score: 1.0,
        title: format!("Album {id}"),
        year: None,
        artists: vec![Named {
            id: artist,
            name: None,
        }],
        label: None,
        styles: Vec::new(),
        reasons: Vec::new(),
    }
}

#[test]
fn the_cap_holds_across_pages() {
    let config = config("cap-pages");
    library(&config, Vec::new());
    // Five by artist 1, then three by artist 2.
    let mut rows: Vec<Row> = (9001..9006).map(|id| by(id, 1)).collect();
    rows.extend((9006..9009).map(|id| by(id, 2)));
    store(&config, &rows);
    let list = open(&config, &Of::Library);
    let pages: Vec<(Vec<Candidate>, bool)> = [0, 2, 4, 6]
        .into_iter()
        .map(|offset| {
            let (albums, more) = page(&list, &Filter::default(), offset, 2);
            (ids(&albums), more)
        })
        .collect();
    let release = Candidate::Release;
    assert_eq!(
        pages,
        [
            (vec![release(9001), release(9002)], true),
            // Artist 1's fourth and fifth are left out, on this page as on the first.
            (vec![release(9003), release(9006)], true),
            (vec![release(9007), release(9008)], false),
            (vec![], false),
        ]
    );
    // A page's albums carry their rank in the stored list.
    let (albums, _) = page(&list, &Filter::default(), 3, 1);
    assert_eq!(albums[0].rank, 6);
}

#[test]
fn a_marked_album_is_left_out_without_a_rebuild() {
    let config = config("marked");
    library(&config, Vec::new());
    let whole = candidates(&lists(&config, Of::Library, false));
    assert!(whole.contains(&Candidate::Release(2003)), "{whole:?}");
    assert!(whole.contains(&Candidate::Master(100)), "{whole:?}");
    // Gus Other's 2003 by its own id, and the band's master 100 by its main release, 1001.
    feedback::set(&config, Kind::Release, 2003, Verdict::NotInterested, 0).unwrap();
    feedback::set(&config, Kind::Release, 1001, Verdict::AlreadyHave, 0).unwrap();
    let marked = candidates(&lists(&config, Of::Library, false));
    assert!(!marked.contains(&Candidate::Release(2003)), "{marked:?}");
    assert!(!marked.contains(&Candidate::Master(100)), "{marked:?}");
    assert_eq!(marked.len(), whole.len() - 2);
    // A seed list leaves them out too, and an undo brings one back.
    let cora = candidates(&lists(&config, Of::Album(2001), false));
    assert!(!cora.contains(&Candidate::Release(2003)), "{cora:?}");
    feedback::undo(&config, Kind::Release, 2003).unwrap();
    let cora = candidates(&lists(&config, Of::Album(2001), false));
    assert!(cora.contains(&Candidate::Release(2003)), "{cora:?}");
}

/// The list `of` opened on `clock`, and what it said on its log.
fn open_logged(config: &Config, clock: &FakeClock, of: &Of) -> (List, Vec<String>) {
    let mut log = Vec::new();
    let list = List::open(config, clock, of, &mut |line| log.push(line.to_string())).unwrap();
    (list, log)
}

fn computed_it(log: &[String]) -> bool {
    log.iter().any(|line| line.contains("computing"))
}

#[test]
fn a_seed_list_is_computed_once_and_read_from_disk_until_the_inputs_change() {
    let config = config("seed-kept");
    library(&config, Vec::new());
    let clock = FakeClock::new();
    let (first, log) = open_logged(&config, &clock, &Of::Album(2001));
    assert!(computed_it(&log), "{log:?}");
    let kept = config.data_dir.join("seed-lists").join("album-2001.sqlite");
    assert!(kept.exists());
    let (page_one, more) = page(&first, &Filter::default(), 0, 1);
    assert!(more);
    drop(first);
    // The next page is read from the file, as a new process or a restarted `serve` reads it.
    let (second, log) = open_logged(&config, &clock, &Of::Album(2001));
    assert!(log.is_empty(), "{log:?}");
    let (page_two, _) = page(&second, &Filter::default(), 1, 1);
    let (both, _) = page(&second, &Filter::default(), 0, 2);
    assert_eq!(ids(&both), [ids(&page_one), ids(&page_two)].concat());
    // An artist's list is kept apart from the album's.
    let (_, log) = open_logged(&config, &clock, &Of::Artist("Cora Seed".to_string()));
    assert!(computed_it(&log), "{log:?}");
    let (_, log) = open_logged(&config, &clock, &Of::Artist("CORA  seed".to_string()));
    assert!(log.is_empty(), "{log:?}");
    // A play more changes the profile's inputs: the list is computed again, the old one gone.
    let mut albums = profile::kept(&config.data_dir).unwrap().unwrap().albums;
    albums[0].plays += 1;
    profile::keep(&Profile { albums }, &config.data_dir).unwrap();
    let (_, log) = open_logged(&config, &clock, &Of::Album(2001));
    assert!(computed_it(&log), "{log:?}");
    let lists = config.data_dir.join("seed-lists");
    let files: Vec<String> = std::fs::read_dir(&lists)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(
        files,
        ["album-2001.sqlite"],
        "the artist's list of the old profile"
    );
}

/// How long a test waits for what it expects, so a missing one fails it.
const WAIT: Duration = Duration::from_secs(10);

#[test]
fn a_seed_list_asked_while_one_is_computed_waits_for_it() {
    let config = config("seed-waits");
    library(&config, Vec::new());
    // A computation holds the lock, in this process or another.
    let computing = catalog::seed_list::lock(&config.data_dir, || {}).unwrap();
    let (said, lines) = mpsc::channel();
    let (done, finished) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| {
            let opened = List::open(&config, &FakeClock::new(), &Of::Album(2001), &mut |line| {
                said.send(line.to_string()).unwrap();
            });
            done.send(opened.is_ok()).unwrap();
        });
        let line = lines.recv_timeout(WAIT).unwrap();
        assert!(line.contains("waiting"), "{line}");
        // It hasn't answered while the other holds the lock; then it computes and answers.
        assert!(finished.try_recv().is_err());
        drop(computing);
        assert!(lines.recv_timeout(WAIT).unwrap().contains("computing"));
        assert_eq!(finished.recv_timeout(WAIT), Ok(true));
    });
}

/// The time a list takes on 1M generated releases with 4,000 seeds spread over them, as in
/// `score.rs`'s budget: the whole library's, read from the stored list, and a seed album's
/// and a seed artist's, each computed from one pass over the catalog the first time and read
/// from disk after; and a page of 50 with each filter (ADR 0028's budget: under 100 ms). Run
/// with `cargo test --release -p features --test list -- --ignored budget --nocapture`.
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
                    3,
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
        for time in ["first", "again"] {
            let started = Instant::now();
            let lists = lists(&config, of.clone(), false);
            println!(
                "{of:?}, {time}: {} albums and {} artists in {:?}",
                lists.albums.len(),
                lists.artists.as_ref().map_or(0, Vec::len),
                started.elapsed()
            );
        }
    }
    let seed_list = config.data_dir.join("seed-lists").join("album-1.sqlite");
    println!(
        "the seed list of album 1 is {} bytes",
        std::fs::metadata(seed_list).unwrap().len()
    );
    time_the_pages(&open(&config, &Of::Library));
    let started = Instant::now();
    let seed = open(&config, &Of::Album(1));
    let page = seed.albums(&Filter::default(), false, 50, 50).unwrap();
    println!(
        "a kept seed list's second page of {} in {:?}, opened",
        page.items.len(),
        started.elapsed()
    );
}

/// Prints how long a page of 50 of `list` takes with each filter, for the budget.
fn time_the_pages(list: &List) {
    use std::time::Instant;
    let label = page(list, &Filter::default(), 0, 1).0[0]
        .label
        .as_ref()
        .unwrap()
        .id;
    let pages: [(&str, Filter, bool, usize); 8] = [
        ("no filter", Filter::default(), false, 0),
        ("no filter, from 1,000", Filter::default(), false, 1000),
        (
            "a style",
            Filter {
                styles: vec!["style 5".to_string()],
                ..Filter::default()
            },
            false,
            0,
        ),
        (
            "a year every album has",
            Filter {
                years: Some((1999, 1999)),
                ..Filter::default()
            },
            false,
            0,
        ),
        (
            "a year no album has",
            Filter {
                years: Some((2000, 2000)),
                ..Filter::default()
            },
            false,
            0,
        ),
        (
            "the first album's label",
            Filter {
                label_ids: vec![label],
                ..Filter::default()
            },
            false,
            0,
        ),
        ("the missing albums", Filter::default(), true, 0),
        (
            "a style, a year and a label",
            Filter {
                styles: vec!["Style 1".to_string(), "Style 5".to_string()],
                years: Some((1990, 1999)),
                label_ids: vec![label, label + 1],
            },
            false,
            0,
        ),
    ];
    for (what, filter, missing, offset) in pages {
        let started = Instant::now();
        let page = list.albums(&filter, missing, offset, 50).unwrap();
        println!(
            "a page of {} with {what} in {:?}",
            page.items.len(),
            started.elapsed()
        );
    }
}
