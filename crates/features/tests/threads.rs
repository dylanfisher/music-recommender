//! Threads on the catalog imported from the sources crate's fixture set: one seed album's
//! links matched against the rest of the kept profile's, as the five kinds, and the
//! refusals.

#[path = "support/catalog.rs"]
mod catalog_support;
#[path = "support/clock.rs"]
mod clock;
#[path = "../../config/tests/support/setup.rs"]
mod support;

use catalog_support::album;
use clock::FakeClock;
use config::Config;
use features::profile::{self, Album, Profile};
use features::rediscover::{self, Id, Kept, Threads, ThreadsError};

/// The fixture catalog in a fresh data directory named for the test.
fn config(test: &str) -> Config {
    catalog_support::imported_config("threads", test, catalog_support::fixture_set)
}

/// Every release of the fixture set, each an album with the release's artist and title.
fn library() -> Vec<Album> {
    [
        (1001, "The Made-Up Band", "First Album"),
        (1002, "The Made-Up Band", "First Album (Reissue)"),
        (1003, "Ann Example", "Untitled"),
        (2001, "Cora Seed", "Seed Album"),
        (2002, "Cora Seed", "Second Album"),
        (2003, "Gus Other", "Produced Album"),
        (2004, "Gus Other", "Cut Album"),
        (2005, "Gus Other", "Sub Album"),
        (2006, "Gus Other", "Late Sub Album"),
        (2008, "DJ Ann", "DJ Album"),
        (2011, "Cora Seed", "Third Album"),
        (2014, "Various", "Made-Up Hits"),
    ]
    .into_iter()
    .map(|(id, artist, title)| album(artist, title, Some(id), 0))
    .collect()
}

/// `config`'s catalog with 2002 by Gus Other instead of Cora Seed. Made-Up Rare is on 2001
/// and 2002 only, both Cora Seed's in the fixture set, and a seed artist's albums are in no
/// `styles` thread (ADR 0036), so seeded with 2001 it is a thread only on this one.
fn rare_on_another_artist(test: &str) -> Config {
    let config = config(test);
    rusqlite::Connection::open(config.data_dir.join("catalog.sqlite"))
        .unwrap()
        .execute(
            "UPDATE release_artist SET artist_id = 23 WHERE release_id = 2002",
            [],
        )
        .unwrap();
    config
}

/// `library` and 30 more albums tagged with 1002, which has neither of Cora Seed's styles,
/// so that Made-Up Rare, on 2 of 42 albums, is under 5% of them and Made-Up Common, on 8,
/// is not.
fn padded() -> Vec<Album> {
    padded_by(30)
}

/// `library` and `extra` more albums tagged with 1002.
fn padded_by(extra: usize) -> Vec<Album> {
    let mut albums = library();
    albums
        .extend((0..extra).map(|n| album("The Made-Up Band", &format!("Copy {n}"), Some(1002), 0)));
    albums
}

fn keep(config: &Config, albums: &[Album]) {
    profile::keep(
        &Profile {
            albums: albums.to_vec(),
        },
        &config.data_dir,
    )
    .unwrap();
}

/// The key of the album tagged with `release_id` in `albums`.
fn key(albums: &[Album], release_id: u64) -> String {
    albums
        .iter()
        .find(|album| album.discogs_release_id == Some(release_id))
        .unwrap()
        .key
        .clone()
}

fn threads(config: &Config, albums: &[Album], release_id: u64) -> Threads {
    rediscover::threads(
        config,
        &FakeClock::new(),
        &Kept::default(),
        &key(albums, release_id),
    )
    .unwrap()
}

/// Each thread as its kind, words and ids.
fn shown(found: &Threads) -> Vec<(String, String, Vec<String>)> {
    found
        .threads
        .iter()
        .map(|thread| {
            (
                thread.kind.name().to_string(),
                thread.words.clone(),
                thread.ids.iter().map(Id::to_string).collect(),
            )
        })
        .collect()
}

fn thread(kind: &str, words: &str, ids: &[&str]) -> (String, String, Vec<String>) {
    (
        kind.to_string(),
        words.to_string(),
        ids.iter().map(|id| (*id).to_string()).collect(),
    )
}

#[test]
fn a_seed_s_artist_people_and_label_are_its_threads() {
    let config = config("credit-label-style");
    let albums = padded();
    keep(&config, &albums);
    let found = threads(&config, &albums, 2001);
    assert_eq!(found.seed.id, Id::Release(2001));
    assert_eq!(found.seed.year, Some(2000));
    assert_eq!(found.computed_at, clock::START);
    // Eli Cutter produced the seed but only cut 2004's lacquer, a weak role, so 2004 is in
    // no thread. Cora Seed sings on 2003, Gus Other's, a thread of its own beside hers; Dee
    // Producer's thread, of 2003 too, repeats it. Made-Up Rare is on 2002 only, Cora Seed's,
    // and Made-Up Common on 8 of the 42 albums, so neither is a thread.
    assert_eq!(
        shown(&found),
        [
            thread(
                "label",
                "Seed Records, 2001 to 2004, on 4 albums you have",
                &["r2014", "r2003", "r2005", "r2006"]
            ),
            thread("credit", "With Cora Seed, on 1 album you have", &["r2003"]),
            thread(
                "artist",
                "By Cora Seed, on 2 albums you have",
                &["r2002", "r2011"]
            ),
        ]
    );
    let label = found.threads[0].label.as_ref().unwrap();
    assert_eq!(
        (label.id, label.name.as_deref()),
        (30, Some("Seed Records"))
    );
    assert_eq!(found.threads[1].artist.as_ref().unwrap().id, 20);
    assert_eq!(found.threads[2].artist.as_ref().unwrap().id, 20);
    // 2003 is in two threads that aren't `artist`, then the others by year, Cora Seed's own
    // last.
    let listed: Vec<String> = found.albums.iter().map(|a| a.id.to_string()).collect();
    assert_eq!(
        listed,
        ["r2003", "r2014", "r2005", "r2006", "r2002", "r2011"]
    );
    let first = &found.albums[0];
    assert_eq!(first.album, albums[5]);
    assert_eq!(first.year, Some(2001));
    assert_eq!(first.label.as_ref().unwrap().id, 30);
}

#[test]
fn the_seed_artist_s_albums_are_in_their_artist_thread_only() {
    let config = config("artist");
    let albums = library();
    keep(&config, &albums);
    let found = threads(&config, &albums, 2003);
    assert_eq!(
        shown(&found),
        [
            thread(
                "credit",
                "Vocals: Cora Seed, on 3 albums you have",
                &["r2001", "r2002", "r2011"]
            ),
            // Gus Other's 2005 and 2006, on Seed Sub, are his thread's only.
            thread(
                "label",
                "Seed Records, 2000 to 2001, on 2 albums you have",
                &["r2001", "r2014"]
            ),
            thread(
                "credit",
                "Producer: Dee Producer, on 1 album you have",
                &["r2001"]
            ),
            thread(
                "artist",
                "By Gus Other, on 3 albums you have",
                &["r2005", "r2006", "r2004"]
            ),
        ]
    );
}

#[test]
fn an_alias_member_or_group_of_the_seed_s_artist_is_a_related_thread() {
    let config = config("related");
    let albums = library();
    keep(&config, &albums);
    let found = threads(&config, &albums, 1003);
    // DJ Ann, an alias of Ann Example, is the seed's artist under another name, so 2008 is
    // in DJ Ann's thread only (ADR 0036).
    assert_eq!(
        shown(&found),
        [
            thread(
                "related",
                "The Made-Up Band, a group Ann Example is in, on 2 albums you have",
                &["r1001", "r1002"]
            ),
            thread(
                "credit",
                "With Ann Example, on 1 album you have",
                &["r1001"]
            ),
            thread(
                "related",
                "DJ Ann, an alias of Ann Example, on 1 album you have",
                &["r2008"]
            ),
        ]
    );
    assert_eq!(found.threads[0].artist.as_ref().unwrap().id, 1);
}

#[test]
fn a_style_on_more_than_5_percent_of_the_tagged_albums_is_no_thread() {
    let config = rare_on_another_artist("common-style");
    // Made-Up Rare is on 2 of the 12.
    let albums = library();
    keep(&config, &albums);
    let found = threads(&config, &albums, 2001);
    assert!(
        found.threads.iter().all(|t| t.kind.name() != "styles"),
        "{:?}",
        shown(&found)
    );
}

#[test]
fn a_style_on_exactly_5_percent_of_the_tagged_albums_is_a_thread_and_just_over_is_not() {
    // Made-Up Rare is on 2 albums: 2 of 40 is exactly 5%, 2 of 39 is just over.
    for (extra, is_thread) in [(28, true), (27, false)] {
        let config = rare_on_another_artist(&format!("rare-boundary-{extra}"));
        let albums = padded_by(extra);
        keep(&config, &albums);
        let found = threads(&config, &albums, 2001);
        let styles = found.threads.iter().any(|t| t.kind.name() == "styles");
        assert_eq!(styles, is_thread, "{extra}: {:?}", shown(&found));
    }
}

#[test]
fn the_seed_is_in_no_thread_and_every_album_takes_part() {
    let config = config("every-album");
    let mut albums = padded();
    // Played this month, and marked Not Now: Rediscover leaves it out, Threads doesn't.
    albums[5].last_played = Some(clock::START - 86_400);
    albums[5].plays = 9;
    keep(&config, &albums);
    rediscover::not_now(
        &config,
        Id::Release(2003),
        clock::START + 86_400,
        clock::START,
    )
    .unwrap();
    for release_id in [
        1001, 1002, 1003, 2001, 2002, 2003, 2004, 2005, 2006, 2008, 2011, 2014,
    ] {
        let found = threads(&config, &albums, release_id);
        let seed = Id::Release(release_id);
        assert!(
            found.threads.iter().all(|t| !t.ids.contains(&seed)),
            "{release_id}: {:?}",
            shown(&found)
        );
        assert!(found.albums.iter().all(|a| a.id != seed), "{release_id}");
    }
    assert!(
        threads(&config, &albums, 2001)
            .albums
            .iter()
            .any(|a| a.id == Id::Release(2003))
    );
}

#[test]
fn a_seed_without_a_profile_a_key_a_catalog_or_a_release_is_refused() {
    let bare = {
        let dir = support::fresh_dir("threads", "refusals");
        let (path, _) = support::write_config(&dir);
        config::load(&path).unwrap()
    };
    let ask = |config: &Config, key: &str| {
        rediscover::threads(config, &FakeClock::new(), &Kept::default(), key).unwrap_err()
    };
    assert!(matches!(ask(&bare, "k0"), ThreadsError::NoProfile));
    let mut albums = library();
    albums.push(album("Made-Up Artist", "Untagged", None, 0));
    albums.push(album("Made-Up Artist", "Not In The Catalog", Some(9999), 0));
    keep(&bare, &albums);
    assert!(matches!(ask(&bare, "no such key"), ThreadsError::NoSeed(_)));
    assert!(matches!(
        ask(&bare, &albums[0].key),
        ThreadsError::NoCatalog
    ));
    let config = config("refusals");
    keep(&config, &albums);
    for album in &albums[12..] {
        let error = ask(&config, &album.key);
        assert!(matches!(error, ThreadsError::Untagged(_)), "{error}");
    }
}
