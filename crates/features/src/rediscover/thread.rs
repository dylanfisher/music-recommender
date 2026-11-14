//! Threads (docs/api.md, Threads; ADR 0035): the library's albums connected to one of them,
//! the seed, grouped by what they share with it. The seed's links, from the seeds' read
//! that Rediscover keeps, matched against every other tagged album's, with no pass over the
//! catalog. Every album of the profile takes part, played recently or not, marked Not Now
//! or not.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;

use catalog::community::{Reading, Stored};
use catalog::discogs::{Catalog, CatalogError};
use catalog::score::ReasonKind;
use config::Config;

use super::relation::{Relations, Thread, is_weak, span, strongest};
use super::{Id, Kept, RediscoverError, first_label};
use crate::clock::{Clock, unix_seconds};
use crate::community::{self, ReadError};
use crate::list::ListError;
use crate::profile::{self, Album, ProfileError};
use crate::reason::{self, count};
use crate::score::{Connection, Named};
use crate::seed::{Relation, Role, Seeds};

// The constants below are first values, which the owner's checkpoint judges (ADR 0035).
/// The most threads a reply.
const THREADS: usize = 12;
/// The most albums a thread lists, but a style's.
const ALBUMS: usize = 60;
/// The most albums a style's thread lists.
const STYLE_ALBUMS: usize = 25;
/// A style of the seed's is a thread only when at most this percent of the profile's tagged
/// albums have it: a common one joins half the library to any seed.
const RARE_PERCENT: usize = 5;

/// One seed's threads.
#[derive(Debug)]
pub struct Threads {
    /// The seed.
    pub seed: Connected,
    /// When the threads were computed, in Unix seconds: at the request.
    pub computed_at: i64,
    /// Every album in a thread, each once, the seed never: the most threads first, then by
    /// year, an album without one last.
    pub albums: Vec<Connected>,
    /// What the albums share with the seed, strongest first, each thread's `ids` by year,
    /// then title.
    pub threads: Vec<Thread>,
}

/// An album of the profile, with what the catalog and the community database hold of it.
#[derive(Debug)]
pub struct Connected {
    /// Its id.
    pub id: Id,
    /// The profile's album.
    pub album: Album,
    /// The release's year in the catalog.
    pub year: Option<u16>,
    /// The release's first label, `None` for none or a "Not On Label".
    pub label: Option<Named>,
    /// The release's styles.
    pub styles: Vec<Named>,
    /// What the community database holds for its release.
    pub community: Option<Stored>,
}

/// Why a seed's threads couldn't be found.
#[derive(Debug)]
pub enum ThreadsError {
    /// No profile has been kept.
    NoProfile,
    /// No album of the kept profile has this key.
    NoSeed(String),
    /// There is no catalog.
    NoCatalog,
    /// The catalog's tables are another version's.
    OtherCatalog,
    /// The album with this key has no release, or one the catalog lacks.
    Untagged(String),
    /// The kept profile can't be read.
    Profile(ProfileError),
    /// The catalog failed.
    Catalog(CatalogError),
    /// The community values can't be read.
    Community(ReadError),
}

impl fmt::Display for ThreadsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoProfile => RediscoverError::NoProfile.fmt(f),
            Self::NoSeed(key) => write!(f, "no album of the profile has the key {key:?}"),
            Self::NoCatalog => ListError::NoCatalog.fmt(f),
            Self::OtherCatalog => ListError::OtherCatalog.fmt(f),
            Self::Untagged(key) => write!(
                f,
                "the album {key:?} has no Discogs release the catalog has, so no threads"
            ),
            Self::Profile(error) => error.fmt(f),
            Self::Catalog(error) => error.fmt(f),
            Self::Community(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for ThreadsError {}

impl From<CatalogError> for ThreadsError {
    fn from(error: CatalogError) -> Self {
        Self::Catalog(error)
    }
}

/// The threads of the album with `key` in the profile kept in `config`'s data directory,
/// at the clock's time, with the seeds' read from `kept`.
///
/// # Errors
///
/// When no profile is kept, no album has `key`, there is no catalog of this version, the
/// album has no release the catalog has, or the profile, the catalog or the community
/// database can't be read.
pub fn threads(
    config: &Config,
    clock: &dyn Clock,
    kept: &Kept,
    key: &str,
) -> Result<Threads, ThreadsError> {
    let now = unix_seconds(clock.now());
    let profile = profile::kept(&config.data_dir)
        .map_err(ThreadsError::Profile)?
        .ok_or(ThreadsError::NoProfile)?;
    let seed_album = profile
        .albums
        .iter()
        .find(|album| album.key == key)
        .ok_or_else(|| ThreadsError::NoSeed(key.to_string()))?;
    let catalog = match Catalog::open_read_only(&config.data_dir)? {
        Reading::Open(catalog) => catalog,
        Reading::Missing => return Err(ThreadsError::NoCatalog),
        Reading::OtherShape => return Err(ThreadsError::OtherCatalog),
    };
    let untagged = || ThreadsError::Untagged(key.to_string());
    let release_id = seed_album.discogs_release_id.ok_or_else(untagged)?;
    let seeds = kept.seeds(&profile, &catalog)?;
    let relations = Relations::new(&seeds, &profile.albums, now);
    let seed = *relations.seed_of.get(&release_id).ok_or_else(untagged)?;
    let tagged = Tagged::new(&relations, &profile.albums);
    let (threads, listed) = matched(&relations, &tagged, seed);
    let mut release_ids = vec![release_id];
    release_ids.extend(listed.iter().map(|&other| seeds.seeds[other].release_id));
    let mut stored = community::stored(config, &release_ids)
        .map_err(ThreadsError::Community)?
        .into_iter();
    let mut labels = HashMap::new();
    let mut connected = |album: &Album, seed: usize| -> Result<Connected, CatalogError> {
        let found = &seeds.seeds[seed];
        Ok(Connected {
            id: Id::Release(found.release_id),
            album: album.clone(),
            year: found.year,
            label: first_label(Some(&catalog), &mut labels, Some(found.release_id))?,
            styles: found
                .styles
                .iter()
                .map(|&style| Named::new(u64::from(style), seeds.style_names.get(&style).cloned()))
                .collect(),
            community: stored.next(),
        })
    };
    let seed = connected(seed_album, seed)?;
    let mut albums = Vec::with_capacity(listed.len());
    for other in listed {
        albums.push(connected(tagged.album(other), other)?);
    }
    Ok(Threads {
        seed,
        computed_at: now,
        albums,
        threads,
    })
}

/// The profile's tagged albums by seed: the first album of each, which stands for the seed
/// in a reply, and how many albums of the profile each is, for the share of a style.
struct Tagged<'a> {
    albums: &'a [Album],
    /// Each seed's first album, by its index in `albums`.
    first: Vec<usize>,
    /// Each seed's albums.
    count: Vec<usize>,
    /// The albums tagged with a seed's release.
    total: usize,
}

impl<'a> Tagged<'a> {
    fn new(relations: &Relations<'_>, albums: &'a [Album]) -> Self {
        let seeds = relations.seeds.seeds.len();
        let mut first = vec![usize::MAX; seeds];
        let mut count = vec![0; seeds];
        for (index, album) in albums.iter().enumerate() {
            let seed = album
                .discogs_release_id
                .and_then(|id| relations.seed_of.get(&id));
            if let Some(&seed) = seed {
                first[seed] = first[seed].min(index);
                count[seed] += 1;
            }
        }
        let total = count.iter().sum();
        Self {
            albums,
            first,
            count,
            total,
        }
    }

    /// The album that stands for `seed`.
    fn album(&self, seed: usize) -> &'a Album {
        &self.albums[self.first[seed]]
    }
}

/// A thread before its caps: its albums by seed.
struct Found {
    kind: ReasonKind,
    /// The thread's words before its count.
    what: String,
    artist: Option<Named>,
    label: Option<Named>,
    seeds: BTreeSet<usize>,
}

/// The threads of `seed`, strongest first, at most `THREADS`, each capped and in its order;
/// and the albums in them by seed, in the reply's order.
fn matched(
    relations: &Relations<'_>,
    tagged: &Tagged<'_>,
    seed: usize,
) -> (Vec<Thread>, Vec<usize>) {
    let seeds = relations.seeds;
    let mut found = people(relations, seed);
    found.extend(related(relations, seed));
    found.extend(labels(relations, seed));
    found.extend(styles(seeds, tagged, seed));
    let seed_of_id: HashMap<Id, usize> = found
        .iter()
        .flat_map(|found| &found.seeds)
        .map(|&other| (Id::Release(seeds.seeds[other].release_id), other))
        .collect();
    let threads: Vec<Thread> = found
        .into_iter()
        .map(|found| Thread {
            kind: found.kind,
            words: format!("{}, on {} you have", found.what, count(found.seeds.len())),
            ids: found
                .seeds
                .iter()
                .map(|&other| Id::Release(seeds.seeds[other].release_id))
                .collect(),
            artist: found.artist,
            label: found.label,
        })
        .collect();
    // By year, then title, an album without a year after those with one.
    let by_year = |&other: &usize| {
        let album = tagged.album(other);
        let year = seeds.seeds[other].year;
        (year.is_none(), year, album.title.clone(), other)
    };
    let mut in_threads: BTreeMap<usize, usize> = BTreeMap::new();
    let mut threads = strongest(threads, 1, THREADS);
    for thread in &mut threads {
        let mut others: Vec<usize> = thread.ids.iter().map(|id| seed_of_id[id]).collect();
        let most = if thread.kind == ReasonKind::Styles {
            STYLE_ALBUMS
        } else {
            ALBUMS
        };
        others.sort_by_key(|&other| (Reverse(tagged.album(other).plays), by_year(&other)));
        others.truncate(most);
        others.sort_by_key(by_year);
        for &other in &others {
            *in_threads.entry(other).or_default() += 1;
        }
        thread.ids = others
            .iter()
            .map(|&other| Id::Release(seeds.seeds[other].release_id))
            .collect();
    }
    let mut listed: Vec<usize> = in_threads.keys().copied().collect();
    listed.sort_by_key(|other| (Reverse(in_threads[other]), by_year(other)));
    (threads, listed)
}

/// The threads of the people on `seed`, a weak role left out on both sides: `Artist` for an
/// artist of the seed on albums all by them, `Credit` for any other.
fn people(relations: &Relations<'_>, seed: usize) -> Vec<Found> {
    let seeds = relations.seeds;
    // Each person on the seed, whether its artist, and their first credit's role.
    let mut on_seed: BTreeMap<u64, (bool, Option<u32>)> = BTreeMap::new();
    for &(person, role) in &relations.parts[seed] {
        let entry = on_seed.entry(person).or_default();
        match role {
            Role::Artist => entry.0 = true,
            Role::Credit(role) => {
                entry.1.get_or_insert(role);
            }
        }
    }
    let mut found = Vec::new();
    for (person, (seed_artist, role)) in on_seed {
        // Each other album, and whether the person is its artist.
        let mut others: BTreeMap<usize, bool> = BTreeMap::new();
        for &(other, part) in &seeds.people[&person].parts {
            if other != seed && !is_weak(seeds, part) {
                *others.entry(other).or_default() |= part == Role::Artist;
            }
        }
        let artist = relations.artist(person);
        let name = reason::name(&artist, "artist");
        let role = role.and_then(|role| seeds.roles.get(&role));
        let (kind, what) = if seed_artist && others.values().all(|&by| by) {
            (ReasonKind::Artist, format!("By {name}"))
        } else {
            match role {
                Some(role) if !seed_artist => {
                    (ReasonKind::Credit, format!("{}: {name}", role.name))
                }
                _ => (ReasonKind::Credit, format!("With {name}")),
            }
        };
        found.push(Found {
            kind,
            what,
            artist: Some(artist),
            label: None,
            seeds: others.into_keys().collect(),
        });
    }
    found
}

/// The threads of the aliases, members and groups of the people on `seed` that are artists
/// of other albums and not on the seed themselves, each on the albums by them.
fn related(relations: &Relations<'_>, seed: usize) -> Vec<Found> {
    let seeds = relations.seeds;
    let on_seed: BTreeSet<u64> = relations.parts[seed]
        .iter()
        .map(|&(person, _)| person)
        .collect();
    // Each related artist, with how: whether the relation was read from the person's side,
    // the relation, and the person.
    let mut ways: BTreeMap<u64, (bool, Relation, u64)> = BTreeMap::new();
    let mut add = |artist: u64, way: (bool, Relation, u64)| {
        if !on_seed.contains(&artist) {
            let entry = ways.entry(artist).or_insert(way);
            *entry = (*entry).min(way);
        }
    };
    for (&artist, between) in &seeds.between {
        for &(relation, person) in between {
            if on_seed.contains(&person) {
                add(artist, (false, relation, person));
            }
        }
    }
    for &person in &on_seed {
        for &(relation, artist) in seeds.between.get(&person).into_iter().flatten() {
            add(artist, (true, relation, person));
        }
    }
    let mut found = Vec::new();
    for (artist, (reversed, relation, person)) in ways {
        let Some(parts) = seeds.people.get(&artist) else {
            continue;
        };
        let (related, of) = (relations.artist(artist), relations.artist(person));
        // `between` says which of the two is the alias, member or group of the other.
        let connection = if reversed {
            Connection::Related {
                artist: of,
                relation,
                of: related.clone(),
            }
        } else {
            Connection::Related {
                artist: related.clone(),
                relation,
                of,
            }
        };
        found.push(Found {
            kind: ReasonKind::Related,
            what: reason::words(&connection),
            artist: Some(related),
            label: None,
            seeds: parts
                .parts
                .iter()
                .filter(|&&(other, part)| other != seed && part == Role::Artist)
                .map(|&(other, _)| other)
                .collect(),
        });
    }
    found
}

/// The threads of `seed`'s labels: each with the albums on its family, the label, its parent
/// and its sublabels, in any year, the years they span in its words.
fn labels(relations: &Relations<'_>, seed: usize) -> Vec<Found> {
    let mut families: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
    for &(family, label, _) in &relations.families[seed] {
        families.entry(label).or_default().push(family);
    }
    let mut found = Vec::new();
    for (label, family) in families {
        let mut years: BTreeMap<usize, u16> = BTreeMap::new();
        for member in family {
            for on in &relations.seeds.families[&member].on {
                if on.label == member && on.seed != seed {
                    years.insert(on.seed, on.year);
                }
            }
        }
        let label = relations.label(label);
        found.push(Found {
            kind: ReasonKind::Label,
            what: format!("{}{}", reason::name(&label, "label"), span(&years)),
            artist: None,
            label: Some(label),
            seeds: years.into_keys().collect(),
        });
    }
    found
}

/// The threads of `seed`'s rare styles, those at most `RARE_PERCENT` of the profile's
/// tagged albums have.
fn styles(seeds: &Seeds, tagged: &Tagged<'_>, seed: usize) -> Vec<Found> {
    let mut found = Vec::new();
    for &style in &seeds.seeds[seed].styles {
        let have: BTreeSet<usize> = (0..seeds.seeds.len())
            .filter(|&other| seeds.seeds[other].styles.contains(&style))
            .collect();
        let albums: usize = have.iter().map(|&other| tagged.count[other]).sum();
        if albums * 100 > tagged.total * RARE_PERCENT {
            continue;
        }
        let name = seeds.style_names.get(&style).map(String::as_str);
        found.push(Found {
            kind: ReasonKind::Styles,
            what: reason::name_or_id(u64::from(style), name, "style"),
            artist: None,
            label: None,
            seeds: have.into_iter().filter(|&other| other != seed).collect(),
        });
    }
    found
}

#[cfg(test)]
mod tests {
    use catalog::score::ReasonKind;

    use super::{ALBUMS, STYLE_ALBUMS, THREADS, Tagged, matched};
    use crate::profile::Album;
    use crate::rediscover::relation::{Relations, Thread};
    use crate::seed::{CreditRole, Family, OnLabel, Person, Role, Seed, Seeds};

    /// A release of the made-up seeds: its artist, the people credited as its producer, its
    /// label and year, and its styles.
    #[derive(Default)]
    struct Release {
        artist: u64,
        credits: Vec<u64>,
        label: Option<u64>,
        year: Option<u16>,
        styles: Vec<u32>,
    }

    /// The seeds of `releases`, release `index + 1` each.
    fn seeds(releases: &[Release]) -> Seeds {
        let mut seeds = Seeds::default();
        seeds.roles.insert(
            1,
            CreditRole {
                name: "Producer".to_string(),
                weight: 1.0,
            },
        );
        for (index, release) in releases.iter().enumerate() {
            seeds.seeds.push(Seed {
                release_id: index as u64 + 1,
                weight: 1.0,
                artists: vec![release.artist],
                styles: release.styles.clone(),
                year: release.year,
            });
            let mut part = |person: u64, role: Role| {
                seeds
                    .people
                    .entry(person)
                    .or_insert_with(|| Person {
                        parts: Vec::new(),
                        releases: 1,
                    })
                    .parts
                    .push((index, role));
            };
            part(release.artist, Role::Artist);
            for &person in &release.credits {
                part(person, Role::Credit(1));
            }
            if let (Some(label), Some(year)) = (release.label, release.year) {
                seeds
                    .families
                    .entry(label)
                    .or_insert_with(|| Family {
                        on: Vec::new(),
                        releases: 1,
                    })
                    .on
                    .push(OnLabel {
                        label,
                        seed: index,
                        year,
                    });
            }
        }
        seeds
    }

    /// An album tagged with release `release_id`, titled `title`, played `plays` times.
    fn album(release_id: u64, title: &str, plays: u64) -> Album {
        Album {
            key: format!("{release_id}/{title}"),
            artist: "A".to_string(),
            title: title.to_string(),
            track_count: 1,
            durations: vec![60],
            catalog_numbers: Vec::new(),
            barcodes: Vec::new(),
            discogs_release_id: Some(release_id),
            plays,
            loved: 0,
            rating: None,
            last_played: None,
            skips: 0,
        }
    }

    /// The threads of the first of `seeds` among `albums`, and the albums in them, by
    /// release id.
    fn threads_of(seeds: &Seeds, albums: &[Album]) -> (Vec<Thread>, Vec<u64>) {
        let relations = Relations::new(seeds, albums, 0);
        let tagged = Tagged::new(&relations, albums);
        let (threads, listed) = matched(&relations, &tagged, 0);
        let listed = listed
            .into_iter()
            .map(|seed| seeds.seeds[seed].release_id)
            .collect();
        (threads, listed)
    }

    /// `thread`'s ids as release ids.
    fn ids(thread: &Thread) -> Vec<u64> {
        thread
            .ids
            .iter()
            .map(|id| match id {
                crate::rediscover::Id::Release(release_id) => *release_id,
                crate::rediscover::Id::Key(_) => panic!("{id} is untagged"),
            })
            .collect()
    }

    /// `n` more albums tagged with release `release_id`, which has none of the seed's styles,
    /// so that a style on a few albums is rare.
    fn padding(release_id: u64, n: usize) -> Vec<Album> {
        (0..n)
            .map(|n| album(release_id, &format!("Pad {n}"), 0))
            .collect()
    }

    #[test]
    fn a_thread_lists_its_most_played_and_its_words_give_the_whole_count() {
        // The seed, then 70 albums by its artist, the first 30 with its style 9.
        let mut releases = vec![Release {
            artist: 1,
            styles: vec![9],
            ..Release::default()
        }];
        releases.extend((1..=70).map(|n| Release {
            artist: 1,
            styles: if n <= 30 { vec![9] } else { Vec::new() },
            ..Release::default()
        }));
        let mut albums: Vec<Album> = (0..=70u64)
            .map(|n| album(n + 1, &format!("Album {n:03}"), n))
            .collect();
        // Style 9 is on 31 of 671 albums, under 5%.
        albums.extend(padding(71, 600));
        let (threads, _) = threads_of(&seeds(&releases), &albums);
        let kinds: Vec<_> = threads.iter().map(|t| (t.kind, t.words.as_str())).collect();
        assert_eq!(
            kinds,
            [
                (ReasonKind::Artist, "By artist 1, on 70 albums you have"),
                (ReasonKind::Styles, "style 9, on 30 albums you have"),
            ]
        );
        // Albums 11 to 70 are the most played 60, 6 to 30 the style's most played 25; each
        // listed by title, as none has a year.
        assert_eq!(ids(&threads[0]), (12..=71).collect::<Vec<u64>>());
        assert_eq!(ids(&threads[0]).len(), ALBUMS);
        assert_eq!(ids(&threads[1]), (7..=31).collect::<Vec<u64>>());
        assert_eq!(ids(&threads[1]).len(), STYLE_ALBUMS);
    }

    #[test]
    fn threads_go_by_their_albums_then_kind_with_styles_last_and_a_repeat_left_out() {
        let releases = vec![
            // The seed.
            Release {
                artist: 1,
                credits: vec![2, 3],
                label: Some(50),
                year: Some(2000),
                styles: vec![7, 8, 9],
            },
            // Albums by its artist.
            Release {
                artist: 1,
                ..Release::default()
            },
            Release {
                artist: 1,
                ..Release::default()
            },
            // Produced by 2.
            Release {
                artist: 10,
                credits: vec![2],
                ..Release::default()
            },
            Release {
                artist: 11,
                credits: vec![2],
                ..Release::default()
            },
            Release {
                artist: 12,
                credits: vec![2],
                ..Release::default()
            },
            // Produced by 3, on the seed's label: the label's thread repeats 3's.
            Release {
                artist: 13,
                credits: vec![3],
                label: Some(50),
                year: Some(2001),
                ..Release::default()
            },
            Release {
                artist: 14,
                credits: vec![3],
                label: Some(50),
                year: Some(2001),
                ..Release::default()
            },
            Release {
                artist: 15,
                credits: vec![3],
                label: Some(50),
                year: Some(2001),
                ..Release::default()
            },
            // Each of the seed's styles on 4 albums, more than any other thread has.
            Release {
                artist: 16,
                styles: vec![7, 8, 9],
                ..Release::default()
            },
            Release {
                artist: 17,
                styles: vec![7, 8, 9],
                ..Release::default()
            },
            Release {
                artist: 18,
                styles: vec![7, 9],
                ..Release::default()
            },
            Release {
                artist: 19,
                styles: vec![7],
                ..Release::default()
            },
            Release {
                artist: 20,
                styles: vec![8, 9],
                ..Release::default()
            },
            Release {
                artist: 21,
                styles: vec![8],
                ..Release::default()
            },
        ];
        let mut albums: Vec<Album> = (1..=releases.len() as u64)
            .map(|id| album(id, &format!("Album {id:02}"), 0))
            .collect();
        albums.extend(padding(2, 100));
        let (threads, _) = threads_of(&seeds(&releases), &albums);
        let shown: Vec<(ReasonKind, Vec<u64>)> = threads.iter().map(|t| (t.kind, ids(t))).collect();
        assert_eq!(
            shown,
            [
                (ReasonKind::Credit, vec![4, 5, 6]),
                (ReasonKind::Credit, vec![7, 8, 9]),
                (ReasonKind::Artist, vec![2, 3]),
                (ReasonKind::Styles, vec![10, 11, 12, 13]),
                (ReasonKind::Styles, vec![10, 11, 14, 15]),
            ]
        );
    }

    #[test]
    fn a_reply_holds_at_most_12_threads() {
        // The seed has 15 producers, each on one other album.
        let mut releases = vec![Release {
            artist: 1,
            credits: (100..115).collect(),
            ..Release::default()
        }];
        releases.extend((100..115).map(|person| Release {
            artist: person + 100,
            credits: vec![person],
            ..Release::default()
        }));
        let albums: Vec<Album> = (1..=releases.len() as u64)
            .map(|id| album(id, "Album", 0))
            .collect();
        let (threads, listed) = threads_of(&seeds(&releases), &albums);
        assert_eq!(threads.len(), THREADS);
        assert_eq!(listed.len(), THREADS);
    }

    #[test]
    fn a_thread_goes_by_year_then_title_and_the_albums_by_their_threads_then_year() {
        let by = |year, credits: Vec<u64>| Release {
            artist: 1,
            credits,
            year,
            ..Release::default()
        };
        let releases = vec![
            Release {
                artist: 1,
                credits: vec![2],
                ..Release::default()
            },
            by(Some(2005), vec![2]),
            by(Some(2001), Vec::new()),
            by(Some(2005), Vec::new()),
            by(None, vec![2]),
        ];
        let albums = vec![
            album(1, "Seed", 0),
            album(2, "B", 0),
            album(3, "Z", 0),
            album(4, "A", 0),
            album(5, "A", 0),
        ];
        let (threads, listed) = threads_of(&seeds(&releases), &albums);
        assert_eq!(threads[0].kind, ReasonKind::Artist);
        assert_eq!(ids(&threads[0]), [3, 4, 2, 5]);
        assert_eq!(ids(&threads[1]), [2, 5]);
        assert_eq!(listed, [2, 5, 3, 4]);
    }
}
