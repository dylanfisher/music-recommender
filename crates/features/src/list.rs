//! The lists of recommendations on the command line (recommendations.md, Lists and
//! evaluation; ADR 0026): the whole library's, read by rank from the stored list, or a seed
//! album's or a seed artist's, scored when asked; all of a scope's albums, or only the
//! missing albums of its artists; at most `CAP` albums of one artist or one label; and the
//! artists of the albums, each by its best three, the library's own artists left out, since
//! they are suggested only through albums of theirs the library doesn't have.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::Path;

use catalog::discogs::{Catalog, CatalogError, Entry, Reading};
use catalog::score::{self as stored, Computed};
use config::Config;
use primitives::fold;

use crate::artist::{Artists, artist_keys, roll_up};
use crate::connection::Connections;
use crate::library::Library;
use crate::profile::{self, Album as LibraryAlbum, Profile, ProfileError};
use crate::reason;
use crate::rescore::{self, RescoreError};
use crate::score::{self, Scored};
use crate::seed::{NOT_ON_LABEL, Seeds};

/// How many albums of one artist, and of one label, a list prints.
const CAP: usize = 3;
/// How many times the albums asked for are read of the stored list at first, since the caps
/// and `--missing` skip some; each read after doubles what is read.
const FIRST_PAGE: u64 = 4;
/// What an album with no row in the catalog is titled.
const NO_TITLE: &str = "no title in the catalog";
/// How many albums, and artists, a list prints unless asked for another number.
pub const LIMIT: usize = 50;

/// Which seeds a list is scored from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Of {
    /// Every seed: the stored list.
    Library,
    /// The seed tagged with this release.
    Album(u64),
    /// The seeds of the library albums by this artist, by its folded name.
    Artist(String),
}

/// A list asked for.
#[derive(Debug, Clone)]
pub struct Ask {
    /// Its seeds.
    pub of: Of,
    /// Only the albums by an artist of the seeds' albums, which the library doesn't have.
    pub missing: bool,
    /// How many albums, and artists, to print.
    pub limit: usize,
}

/// A list of albums, and of artists, ready to print.
#[derive(Debug)]
pub struct Lists {
    /// What the list is of.
    heading: String,
    /// Its albums, best first, at most `CAP` of one artist or label.
    pub albums: Vec<Album>,
    /// Its artists, best first; `None` for the missing albums, whose artists are all the
    /// library's.
    pub artists: Option<Vec<Artist>>,
}

/// A candidate of a list, with what the catalog says of it.
#[derive(Debug, Clone)]
pub struct Album {
    /// The candidate, its score and its reasons.
    pub scored: Scored,
    /// Its title, year and artists, `None` for a release the catalog has no row for.
    pub entry: Option<Entry>,
    /// The release its label is read from, `None` when the catalog has no row for the
    /// album's master.
    representative: Option<u64>,
}

/// An artist of a list's albums.
#[derive(Debug, Clone)]
pub struct Artist {
    /// Its Discogs id.
    pub id: u64,
    /// Its name, `None` when the catalog has no row for it.
    pub name: Option<String>,
    /// Its best album's score, with a quarter of its second's and a tenth of its third's,
    /// from 0 to 1 for the best artist.
    pub score: f64,
    /// The albums its score counts, at most 3.
    pub albums: usize,
    /// Its best album's title and strongest reason's words.
    best: (String, Option<String>),
}

/// Why a list can't be made.
#[derive(Debug)]
pub enum ListError {
    /// There is no catalog to read the albums from.
    NoCatalog,
    /// The catalog's tables are another version's.
    OtherCatalog,
    /// No profile has been kept, so there are no seeds.
    NoProfile,
    /// No list of the whole library has been stored.
    NoList,
    /// The stored list was rebuilt while it was read by pages.
    Rebuilt,
    /// The stored list was computed from another catalog or profile than the current ones.
    Stale {
        /// Whether a rebuild is running now.
        rebuilding: bool,
    },
    /// No album of the kept profile is tagged with the release.
    NotInProfile(u64),
    /// The release a profile album is tagged with isn't in the catalog, so it is no seed.
    NotInCatalog(u64),
    /// No album of the kept profile is by the artist.
    NoAlbumBy(String),
    /// The artist's albums in the kept profile are tagged with no release the catalog has.
    NoSeedBy(String),
    /// The catalog failed.
    Catalog(CatalogError),
    /// The kept profile can't be read.
    Profile(ProfileError),
    /// The stored list can't be read.
    Rescore(RescoreError),
}

impl fmt::Display for ListError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoCatalog => f.write_str("no catalog yet; `dumps import` builds one"),
            Self::OtherCatalog => f.write_str(
                "the catalog's tables are from another version; the next import replaces it",
            ),
            Self::NoProfile => f.write_str(
                "no profile kept yet; POST /v1/profile or `profile load <file>` keeps one",
            ),
            Self::NoList => f.write_str(
                "no list of the whole library yet; `profile load <file>` scores one, as serve \
                 does after a profile or an import",
            ),
            Self::Rebuilt => {
                f.write_str("the list was rebuilt while it was read; ask for it again")
            }
            Self::Stale { rebuilding: true } => f.write_str(
                "the list is out of date for the current catalog or profile, and a rebuild is \
                 running; try again when it ends",
            ),
            Self::Stale { rebuilding: false } => f.write_str(
                "the list is out of date for the current catalog or profile, and no rebuild is \
                 running; `profile load <file>` rebuilds it now, or `serve` does at its start",
            ),
            Self::NotInProfile(id) => {
                write!(
                    f,
                    "no album of the kept profile is tagged with release {id}"
                )
            }
            Self::NotInCatalog(id) => write!(
                f,
                "release {id} is in the kept profile but not in the catalog, so it is no seed"
            ),
            Self::NoAlbumBy(name) => write!(f, "no album of the kept profile is by `{name}`"),
            Self::NoSeedBy(name) => write!(
                f,
                "no album by `{name}` is tagged with a release the catalog has, so it has no \
                 seed"
            ),
            Self::Catalog(error) => error.fmt(f),
            Self::Profile(error) => error.fmt(f),
            Self::Rescore(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for ListError {}

impl From<CatalogError> for ListError {
    fn from(error: CatalogError) -> Self {
        Self::Catalog(error)
    }
}

impl From<ProfileError> for ListError {
    fn from(error: ProfileError) -> Self {
        Self::Profile(error)
    }
}

impl From<RescoreError> for ListError {
    fn from(error: RescoreError) -> Self {
        Self::Rescore(error)
    }
}

/// The lists `ask` asks for, from the catalog, the kept profile and, for the whole library,
/// the stored list in `config`'s data directory. A seed album's or artist's list is scored
/// from one pass over the catalog.
///
/// # Errors
///
/// When there is no catalog, kept profile or, for the whole library, stored list; when the
/// album or artist asked for has no seed; and when a read fails.
pub fn lists(config: &Config, ask: &Ask) -> Result<Lists, ListError> {
    let data_dir = &config.data_dir;
    let catalog = match Catalog::open_read_only(data_dir)? {
        Reading::Missing => return Err(ListError::NoCatalog),
        Reading::OtherShape => return Err(ListError::OtherCatalog),
        Reading::Open(catalog) => catalog,
    };
    let profile = profile::kept(data_dir)?.ok_or(ListError::NoProfile)?;
    // Each arm yields the albums whose artists `--missing` keeps; `None` for all of them.
    let (heading, mut ranked, scope) = match &ask.of {
        Of::Library => {
            let ranked = Ranked::stored(data_dir, &catalog, ask.limit)?;
            // `stored` read the first page, which says what the list was computed from.
            let computed = ranked.computed.as_ref().ok_or(ListError::NoList)?;
            if computed.inputs != rescore::inputs(&catalog, &profile)? {
                let rebuilding = stored::running(data_dir).map_err(RescoreError::from)?;
                return Err(ListError::Stale { rebuilding });
            }
            (whole_heading(computed), ranked, None)
        }
        Of::Album(release_id) => {
            let albums: Vec<&LibraryAlbum> = profile
                .albums
                .iter()
                .filter(|album| album.discogs_release_id == Some(*release_id))
                .collect();
            let Some(first) = albums.first() else {
                return Err(ListError::NotInProfile(*release_id));
            };
            let heading = format!(
                "the seed album {release_id}: {} – {}",
                first.artist, first.title
            );
            let (ranked, _) = seed_list(&profile, &catalog, &albums, || {
                ListError::NotInCatalog(*release_id)
            })?;
            (heading, ranked, Some(albums))
        }
        Of::Artist(name) => {
            let key = fold::key(name, false);
            let albums: Vec<&LibraryAlbum> = profile
                .albums
                .iter()
                .filter(|album| artist_keys(&album.artist).any(|artist| artist == key))
                .collect();
            if albums.is_empty() {
                return Err(ListError::NoAlbumBy(name.clone()));
            }
            let (ranked, seeds) = seed_list(&profile, &catalog, &albums, || {
                ListError::NoSeedBy(name.clone())
            })?;
            let heading = format!(
                "the {seeds} seed album{} by {name}",
                if seeds == 1 { "" } else { "s" }
            );
            (heading, ranked, Some(albums))
        }
    };
    let heading = if ask.missing {
        format!("{heading}\nonly the missing albums of its artists")
    } else {
        heading
    };
    // Only `--missing` reads the scope's artists; the whole library's are the profile's.
    let scope_artists = ask
        .missing
        .then(|| {
            let scope = scope.unwrap_or_else(|| profile.albums.iter().collect());
            Artists::read(scope.into_iter(), &catalog)
        })
        .transpose()?;
    let keep = |album: &Album| {
        !ask.missing
            || album.entry.as_ref().is_some_and(|entry| {
                scope_artists
                    .as_ref()
                    .is_some_and(|artists| artists.has_any(entry))
            })
    };
    let albums = capped(
        &mut ranked,
        &keep,
        &mut |album| label(&catalog, album),
        ask.limit,
    )?;
    let artists = if ask.missing {
        None
    } else if matches!(ask.of, Of::Library) {
        Some(stored_artists(data_dir, &catalog, &ranked, ask.limit)?)
    } else {
        // The seeds' albums are all in memory, so the roll-up reads every one. The
        // library's artists are the whole profile's, not the scope's.
        let library_artists = Artists::read(profile.albums.iter(), &catalog)?;
        Some(rolled(&ranked, &library_artists, ask.limit))
    };
    Ok(Lists {
        heading,
        albums,
        artists,
    })
}

/// The heading of the whole library's list.
fn whole_heading(computed: &Computed) -> String {
    format!(
        "the whole library: the list computed from the set of {} and {} seeds, {} candidates",
        computed.inputs.catalog_set, computed.seeds, computed.candidates
    )
}

/// The candidates of the seeds `albums` are tagged with, scored when asked, and how many
/// seeds those are; `none` when no seed is among them. The seeds are read for those albums
/// alone, so the pass keeps only what connects to them and their weights are scaled
/// among themselves.
fn seed_list<'a>(
    profile: &Profile,
    catalog: &'a Catalog,
    albums: &[&LibraryAlbum],
    none: impl FnOnce() -> ListError,
) -> Result<(Ranked<'a>, usize), ListError> {
    let their_albums = Profile {
        albums: albums.iter().map(|&album| album.clone()).collect(),
    };
    let seeds = Seeds::read(&their_albums, catalog)?;
    if seeds.seeds.is_empty() {
        return Err(none());
    }
    let connections = Connections::read(catalog, &seeds)?;
    // The whole profile's albums are still the library's, and never suggested.
    let library = Library::new(profile, catalog)?;
    let albums = score::score(&seeds, connections, &library)
        .map_err(|error| catalog.inconsistent(error.to_string()))?
        .into_iter()
        .map(|(scored, entry)| Album {
            representative: scored.candidate.representative(&entry),
            scored,
            entry: Some(entry),
        })
        .collect();
    let ranked = Ranked {
        albums,
        stored: None,
        computed: None,
    };
    Ok((ranked, seeds.seeds.len()))
}

/// A list's candidates in rank order, the stored list read further by pages as they are
/// asked for.
struct Ranked<'a> {
    albums: Vec<Album>,
    /// Where to read more: `None` once all of the stored list is read, and for a seed
    /// list, which is all in memory.
    stored: Option<(&'a Path, &'a Catalog)>,
    /// What the stored list was computed from, so a page of another list is refused.
    computed: Option<Computed>,
}

impl<'a> Ranked<'a> {
    /// The first page of the stored list in `data_dir`, for a list of `limit` albums.
    fn stored(data_dir: &'a Path, catalog: &'a Catalog, limit: usize) -> Result<Self, ListError> {
        let mut ranked = Self {
            albums: Vec::new(),
            stored: Some((data_dir, catalog)),
            computed: None,
        };
        let first = (limit as u64).saturating_mul(FIRST_PAGE).max(1);
        ranked.read(data_dir, catalog, first)?;
        Ok(ranked)
    }

    /// The album at `index`, reading the stored list further when it has more.
    fn get(&mut self, index: usize) -> Result<Option<&Album>, ListError> {
        while index >= self.albums.len() {
            let Some((data_dir, catalog)) = self.stored else {
                break;
            };
            let read = self.albums.len() as u64;
            self.read(data_dir, catalog, read.max(1))?;
        }
        Ok(self.albums.get(index))
    }

    /// Reads up to `want` candidates of the stored list after the ones read, and adds them.
    /// The caller passes where `stored` points, so a read past the list's end can't be asked.
    fn read(&mut self, data_dir: &Path, catalog: &Catalog, want: u64) -> Result<(), ListError> {
        let list =
            rescore::page(data_dir, self.albums.len() as u64, want)?.ok_or(ListError::NoList)?;
        match &self.computed {
            Some(computed) if *computed != list.computed => return Err(ListError::Rebuilt),
            Some(_) => {}
            None => self.computed = Some(list.computed),
        }
        if (list.candidates.len() as u64) < want {
            self.stored = None;
        }
        for listed in list.candidates {
            let entry = listed.scored.candidate.entry(catalog)?;
            self.albums.push(Album {
                scored: listed.scored,
                entry,
                representative: listed.representative,
            });
        }
        Ok(())
    }
}

/// The first `limit` artists of the stored list's roll-up, each with its best album's title
/// and strongest reason; `ranked` says what the albums were read from, so a roll-up of
/// another rebuild is refused.
fn stored_artists(
    data_dir: &Path,
    catalog: &Catalog,
    ranked: &Ranked<'_>,
    limit: usize,
) -> Result<Vec<Artist>, ListError> {
    let (computed, artists) =
        rescore::top_artists(data_dir, limit as u64)?.ok_or(ListError::NoList)?;
    if ranked.computed.as_ref() != Some(&computed) {
        return Err(ListError::Rebuilt);
    }
    artists
        .into_iter()
        .map(|(artist, best)| {
            let title = match best.scored.candidate.entry(catalog)? {
                Some(entry) => entry.title,
                None => NO_TITLE.to_string(),
            };
            Ok(Artist {
                id: artist.id,
                name: artist.name,
                score: artist.score,
                albums: usize::try_from(artist.albums)
                    .map_err(|_| RescoreError::Reason("too many albums for an artist"))?,
                best: (title, strongest(&best.scored)),
            })
        })
        .collect()
}

/// The first `limit` artists of a seed list, rolled up from all of its albums, which are
/// in memory, leaving out the library's.
fn rolled(ranked: &Ranked<'_>, library_artists: &Artists, limit: usize) -> Vec<Artist> {
    let mut artists = roll_up(
        ranked.albums.iter().map(|album| {
            album
                .entry
                .as_ref()
                .map(|entry| (entry, album.scored.score))
        }),
        &|id, name| library_artists.has(id, name),
    );
    artists.truncate(limit);
    artists
        .into_iter()
        .map(|artist| {
            // `best` is the place, in the albums `roll_up` was given, of the artist's best.
            let album = &ranked.albums[artist.best];
            let title = album
                .entry
                .as_ref()
                .map_or_else(|| NO_TITLE.to_string(), |entry| entry.title.clone());
            Artist {
                id: artist.id,
                name: artist.name,
                score: artist.score,
                albums: artist.albums,
                best: (title, strongest(&album.scored)),
            }
        })
        .collect()
}

/// The words of a candidate's strongest reason, if it has one.
fn strongest(scored: &Scored) -> Option<String> {
    scored
        .reasons
        .first()
        .map(|reason| reason::words(&reason.connection))
}

/// The label an album is capped by: its representative's first label, unless that is
/// Discogs's placeholder for none.
fn label(catalog: &Catalog, album: &Album) -> Result<Option<u64>, ListError> {
    let Some(label) = album
        .representative
        .map(|id| catalog.first_label(id))
        .transpose()?
        .flatten()
    else {
        return Ok(None);
    };
    let placeholder = catalog
        .label(label)?
        .is_some_and(|(name, _)| name.to_lowercase().contains(NOT_ON_LABEL));
    Ok((!placeholder).then_some(label))
}

/// The first `limit` albums of `ranked` that `keep` keeps, leaving out each album one of
/// whose artists, or whose label, has `CAP` albums in the list already.
fn capped(
    ranked: &mut Ranked<'_>,
    keep: &dyn Fn(&Album) -> bool,
    label: &mut dyn FnMut(&Album) -> Result<Option<u64>, ListError>,
    limit: usize,
) -> Result<Vec<Album>, ListError> {
    let mut by_artist: HashMap<u64, usize> = HashMap::new();
    let mut by_label: HashMap<u64, usize> = HashMap::new();
    let mut albums = Vec::new();
    let mut index = 0;
    while albums.len() < limit {
        let Some(album) = ranked.get(index)? else {
            break;
        };
        index += 1;
        if !keep(album) {
            continue;
        }
        let artists: HashSet<u64> = album
            .entry
            .iter()
            .flat_map(|entry| entry.artists.iter().map(|&(id, _)| id))
            .collect();
        if artists
            .iter()
            .any(|id| by_artist.get(id).is_some_and(|&n| n >= CAP))
        {
            continue;
        }
        let album_label = label(album)?;
        if album_label.is_some_and(|id| by_label.get(&id).is_some_and(|&n| n >= CAP)) {
            continue;
        }
        for id in artists {
            *by_artist.entry(id).or_default() += 1;
        }
        if let Some(id) = album_label {
            *by_label.entry(id).or_default() += 1;
        }
        albums.push(album.clone());
    }
    Ok(albums)
}

impl fmt::Display for Lists {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{}", self.heading)?;
        writeln!(f, "\nalbums")?;
        if self.albums.is_empty() {
            writeln!(f, "  none")?;
        }
        for (rank, album) in self.albums.iter().enumerate() {
            let (kind, id) = album.scored.candidate.kind_id();
            let what = match &album.entry {
                Some(entry) => {
                    let artists: Vec<String> = entry
                        .artists
                        .iter()
                        .map(|(id, name)| name.clone().unwrap_or_else(|| format!("artist {id}")))
                        .collect();
                    let year = entry
                        .year
                        .map_or_else(String::new, |year| format!(" ({year})"));
                    format!("{} – {}{year}", artists.join(" / "), entry.title)
                }
                None => NO_TITLE.to_string(),
            };
            writeln!(
                f,
                "{:>3}. {:.2}  {what}, {} {id}",
                rank + 1,
                album.scored.score,
                kind.name()
            )?;
            let reasons: Vec<String> = album
                .scored
                .reasons
                .iter()
                .map(|reason| reason::words(&reason.connection))
                .collect();
            writeln!(f, "        {}", reasons.join("; "))?;
        }
        let Some(artists) = &self.artists else {
            return Ok(());
        };
        writeln!(f, "\nartists")?;
        if artists.is_empty() {
            writeln!(f, "  none")?;
        }
        for (rank, artist) in artists.iter().enumerate() {
            let name = artist
                .name
                .clone()
                .unwrap_or_else(|| format!("artist {}", artist.id));
            writeln!(
                f,
                "{:>3}. {:.2}  {name}, artist {}, from {} album{}",
                rank + 1,
                artist.score,
                artist.id,
                artist.albums,
                if artist.albums == 1 { "" } else { "s" }
            )?;
            let (title, reason) = &artist.best;
            match reason {
                Some(reason) => writeln!(f, "        best: {title}; {reason}")?,
                None => writeln!(f, "        best: {title}")?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use catalog::discogs::Entry;

    use super::{Album, CAP, ListError, Ranked, capped};
    use crate::candidate::Candidate;
    use crate::score::Scored;

    /// A release `id` by `artists`, scored `score`.
    fn album(id: u64, artists: &[u64], score: f64) -> Album {
        Album {
            scored: Scored {
                candidate: Candidate::Release(id),
                score,
                reasons: Vec::new(),
            },
            entry: Some(Entry {
                title: format!("Album {id}"),
                year: None,
                artists: artists.iter().map(|&id| (id, None)).collect(),
                styles: Vec::new(),
                main_release: None,
            }),
            representative: Some(id),
        }
    }

    fn ranked(albums: Vec<Album>) -> Ranked<'static> {
        Ranked {
            albums,
            stored: None,
            computed: None,
        }
    }

    /// The releases `capped` keeps of `albums`, each labelled as `labels` says.
    fn kept(albums: Vec<Album>, labels: &HashMap<u64, u64>, limit: usize) -> Vec<u64> {
        let mut label = |album: &Album| -> Result<Option<u64>, ListError> {
            let Candidate::Release(id) = album.scored.candidate else {
                unreachable!()
            };
            Ok(labels.get(&id).copied())
        };
        capped(&mut ranked(albums), &|_| true, &mut label, limit)
            .unwrap()
            .iter()
            .map(|album| album.scored.candidate.kind_id().1)
            .collect()
    }

    #[test]
    fn one_artist_takes_at_most_cap_albums() {
        // Five by artist 1, one of them with artist 2, then one by artist 2.
        let albums = vec![
            album(10, &[1], 0.9),
            album(11, &[1], 0.8),
            album(12, &[1, 2], 0.7),
            album(13, &[1], 0.6),
            album(14, &[1], 0.5),
            album(15, &[2], 0.4),
        ];
        assert_eq!(CAP, 3);
        assert_eq!(kept(albums, &HashMap::new(), 10), [10, 11, 12, 15]);
    }

    #[test]
    fn one_label_takes_at_most_cap_albums() {
        let albums = (10..15).map(|id| album(id, &[id], 1.0)).collect();
        let labels: HashMap<u64, u64> = (10..15).map(|id| (id, 7)).collect();
        assert_eq!(kept(albums, &labels, 10), [10, 11, 12]);
        // No label, no cap; and the limit still holds.
        let albums = (10..15).map(|id| album(id, &[id], 1.0)).collect();
        assert_eq!(kept(albums, &HashMap::new(), 4), [10, 11, 12, 13]);
    }

    #[test]
    fn a_limit_of_0_lists_nothing() {
        let albums = || (10..80).map(|id| album(id, &[id], 1.0)).collect();
        assert_eq!(kept(albums(), &HashMap::new(), 0), Vec::<u64>::new());
    }
}
