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
use catalog::score::Computed;
use config::Config;
use primitives::fold;

use crate::candidate::Candidate;
use crate::connection::Connections;
use crate::library::{self, Library};
use crate::profile::{self, Album as LibraryAlbum, Profile, ProfileError};
use crate::reason;
use crate::rescore::{self, RescoreError};
use crate::score::{self, Scope, Scored, VARIOUS};
use crate::seed::{NOT_ON_LABEL, Seeds};

/// How many albums of one artist, and of one label, a list prints.
const CAP: usize = 3;
/// What an artist's best, second and third albums add to its score.
const SHARES: [f64; 3] = [1.0, 0.25, 0.1];
/// How many candidates of the stored list are read at first; each read after doubles it.
const PAGE: u64 = 500;
/// How many albums, and artists, a list prints unless asked for another number.
pub const LIMIT: usize = 50;
/// The prototype's word that joins a featured artist to an album's artist.
const FEATURING: &str = " feat. ";

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
    /// Its title, year and artists, `None` for a master the catalog has no row for.
    pub entry: Option<Entry>,
    /// The release its label is read from.
    representative: Representative,
}

/// An album's representative, which the stored list holds and a seed list reads when the
/// cap needs it.
#[derive(Debug, Clone, Copy)]
enum Representative {
    /// Not read yet.
    Unread,
    /// Read: `None` when the catalog has no row for the album's master.
    Read(Option<u64>),
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
    Stale,
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
            Self::Stale => f.write_str(
                "the list is being rebuilt; try again when `sync status` shows it current",
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
    let library_artists = Artists::read(profile.albums.iter(), &catalog)?;
    let (heading, mut ranked, scope_artists) = match &ask.of {
        Of::Library => {
            let (ranked, computed) = Ranked::stored(data_dir, &catalog)?;
            if computed.inputs != rescore::inputs(&catalog, &profile)? {
                return Err(ListError::Stale);
            }
            (whole_heading(&computed), ranked, None)
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
            let artists = Artists::read(albums.iter().copied(), &catalog)?;
            (heading, ranked, Some(artists))
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
            let artists = Artists::read(albums.iter().copied(), &catalog)?;
            (heading, ranked, Some(artists))
        }
    };
    let scope_artists = scope_artists.as_ref().unwrap_or(&library_artists);
    let heading = if ask.missing {
        format!("{heading}\nonly the missing albums of its artists")
    } else {
        heading
    };
    let keep = |album: &Album| !ask.missing || scope_artists.has_any(album);
    let albums = capped(
        &mut ranked,
        &keep,
        &mut |album| label(&catalog, album),
        ask.limit,
    )?;
    let artists = if ask.missing {
        None
    } else {
        Some(roll_up(
            &mut ranked,
            &|id, name| library_artists.has(id, name),
            ask.limit,
        )?)
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
/// alone, so the pass keeps only what connects to them: their weights scaled among
/// themselves score as `Scope::Releases` of them would among the whole profile's seeds.
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
    let scored = score::score(&seeds, &connections, &library, &Scope::Library);
    let albums = scored
        .into_iter()
        .map(|scored| {
            let entry = connections.entries.get(&scored.candidate).cloned();
            Album {
                scored,
                entry,
                representative: Representative::Unread,
            }
        })
        .collect();
    let ranked = Ranked {
        albums,
        stored: None,
        computed: None,
    };
    Ok((ranked, seeds.seeds.len()))
}

/// The keys an album artist's name matches by: the whole name folded, and each name it
/// joins with "feat.", as the prototype's lists matched.
fn artist_keys(name: &str) -> impl Iterator<Item = String> + '_ {
    let parts = name
        .contains(FEATURING)
        .then(|| name.split(FEATURING))
        .into_iter()
        .flatten();
    std::iter::once(name)
        .chain(parts)
        .filter_map(library::artist_key)
}

/// The artists of some library albums: the Discogs artists of the releases they are tagged
/// with, and their own artist names folded.
#[derive(Debug)]
struct Artists {
    ids: HashSet<u64>,
    names: HashSet<String>,
}

impl Artists {
    fn read<'a>(
        albums: impl Iterator<Item = &'a LibraryAlbum>,
        catalog: &Catalog,
    ) -> Result<Self, CatalogError> {
        let mut artists = Self {
            ids: HashSet::new(),
            names: HashSet::new(),
        };
        for album in albums {
            artists.names.extend(artist_keys(&album.artist));
            if let Some(release_id) = album.discogs_release_id
                && let Some(links) = catalog.links(release_id)?
            {
                artists
                    .ids
                    .extend(links.artists.into_iter().filter(|&id| id != VARIOUS));
            }
        }
        Ok(artists)
    }

    /// Whether the artist `id`, named `name`, is one of these.
    fn has(&self, id: u64, name: Option<&str>) -> bool {
        self.ids.contains(&id)
            || name
                .and_then(library::artist_key)
                .is_some_and(|key| self.names.contains(&key))
    }

    /// Whether any of `album`'s artists is one of these.
    fn has_any(&self, album: &Album) -> bool {
        album.entry.as_ref().is_some_and(|entry| {
            entry
                .artists
                .iter()
                .any(|(id, name)| self.has(*id, name.as_deref()))
        })
    }
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
    /// The first page of the stored list in `data_dir`, and what the list was computed
    /// from.
    fn stored(data_dir: &'a Path, catalog: &'a Catalog) -> Result<(Self, Computed), ListError> {
        let mut ranked = Self {
            albums: Vec::new(),
            stored: Some((data_dir, catalog)),
            computed: None,
        };
        let computed = ranked.read(PAGE)?;
        Ok((ranked, computed))
    }

    /// The album at `index`, reading the stored list further when it has more.
    fn get(&mut self, index: usize) -> Result<Option<&Album>, ListError> {
        while index >= self.albums.len() && self.stored.is_some() {
            let read = self.albums.len() as u64;
            self.read((read * 2).max(PAGE))?;
        }
        Ok(self.albums.get(index))
    }

    /// Reads the stored list's first `want` candidates and adds the ones not read yet;
    /// returns what the list was computed from.
    fn read(&mut self, want: u64) -> Result<Computed, ListError> {
        let (data_dir, catalog) = self
            .stored
            .unwrap_or_else(|| panic!("the stored list read after its end"));
        let list = rescore::top(data_dir, want)?.ok_or(ListError::NoList)?;
        match &self.computed {
            Some(computed) if *computed != list.computed => return Err(ListError::Rebuilt),
            Some(_) => {}
            None => self.computed = Some(list.computed.clone()),
        }
        if (list.candidates.len() as u64) < want {
            self.stored = None;
        }
        for listed in list.candidates.into_iter().skip(self.albums.len()) {
            let entry = match listed.scored.candidate {
                Candidate::Master(id) => catalog.master_entry(id)?,
                Candidate::Release(id) => catalog.release_entry(id)?,
            };
            self.albums.push(Album {
                scored: listed.scored,
                entry,
                representative: Representative::Read(listed.representative),
            });
        }
        Ok(list.computed)
    }
}

/// The label an album is capped by: its representative's first label, unless that is
/// Discogs's placeholder for none.
fn label(catalog: &Catalog, album: &Album) -> Result<Option<u64>, ListError> {
    let representative = match album.representative {
        Representative::Read(representative) => representative,
        Representative::Unread => album.scored.candidate.representative(catalog)?,
    };
    let Some(label) = representative
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

/// The artists of `ranked`'s albums, each scored by its best three, scaled to the best
/// artist's, the first `limit` of them, leaving out Various and those `excluded` says.
///
/// The albums are read in rank order, and the roll-up stops before an album when `settled`
/// says the listed artists and their order can't change any more. An artist's second and
/// third albums below that point aren't counted, which is at most a third of the score
/// where reading stops.
fn roll_up(
    ranked: &mut Ranked<'_>,
    excluded: &dyn Fn(u64, Option<&str>) -> bool,
    limit: usize,
) -> Result<Vec<Artist>, ListError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut artists: Vec<Artist> = Vec::new();
    let mut by_id: HashMap<u64, usize> = HashMap::new();
    let mut index = 0;
    while let Some(album) = ranked.get(index)? {
        if settled(&artists, limit, album.scored.score) {
            break;
        }
        index += 1;
        let Some(entry) = &album.entry else {
            continue;
        };
        let mut seen = HashSet::new();
        for (id, name) in &entry.artists {
            if *id == VARIOUS || !seen.insert(*id) || excluded(*id, name.as_deref()) {
                continue;
            }
            let at = *by_id.entry(*id).or_insert_with(|| {
                artists.push(Artist {
                    id: *id,
                    name: name.clone(),
                    score: 0.0,
                    albums: 0,
                    best: (
                        entry.title.clone(),
                        album
                            .scored
                            .reasons
                            .first()
                            .map(|reason| reason::words(&reason.connection)),
                    ),
                });
                artists.len() - 1
            });
            let artist = &mut artists[at];
            if let Some(share) = SHARES.get(artist.albums) {
                artist.score += share * album.scored.score;
                artist.albums += 1;
            }
        }
    }
    artists.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.id.cmp(&b.id)));
    artists.truncate(limit);
    if let Some(best) = artists.first().map(|artist| artist.score)
        && best > 0.0
    {
        for artist in &mut artists {
            artist.score /= best;
        }
    }
    Ok(artists)
}

/// Whether `artists`, read from albums that all score at least `next`, and the rest scoring
/// at most `next`, are final in the first `limit` and their order: every other artist, read
/// or not, ends below the `limit`th, and every listed artist ends above the ones after it.
/// An artist can still gain `next` times the shares of the albums it has left of its three.
fn settled(artists: &[Artist], limit: usize, next: f64) -> bool {
    if artists.len() < limit {
        return false;
    }
    let ahead = |artist: &Artist| {
        artist.score
            + next
                * SHARES[artist.albums.min(SHARES.len())..]
                    .iter()
                    .sum::<f64>()
    };
    let by_score = |a: &&Artist, b: &&Artist| b.score.total_cmp(&a.score).then(a.id.cmp(&b.id));
    let mut all: Vec<&Artist> = artists.iter().collect();
    let (listed, nth, rest) = {
        let (listed, nth, rest) = all.select_nth_unstable_by(limit - 1, by_score);
        (listed, *nth, rest)
    };
    if next * SHARES.iter().sum::<f64>() >= nth.score
        || rest.iter().any(|artist| ahead(artist) >= nth.score)
    {
        return false;
    }
    listed.sort_by(by_score);
    let mut after = ahead(nth);
    listed.iter().rev().all(|artist| {
        let above = artist.score > after;
        after = after.max(ahead(artist));
        above
    })
}

impl fmt::Display for Lists {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{}", self.heading)?;
        writeln!(f, "\nalbums")?;
        if self.albums.is_empty() {
            writeln!(f, "  none")?;
        }
        for (rank, album) in self.albums.iter().enumerate() {
            let (kind, id) = match album.scored.candidate {
                Candidate::Master(id) => ("master", id),
                Candidate::Release(id) => ("release", id),
            };
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
                None => "no title in the catalog".to_string(),
            };
            writeln!(
                f,
                "{:>3}. {:.2}  {what}, {kind} {id}",
                rank + 1,
                album.scored.score
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

    use super::{Album, CAP, ListError, Ranked, Representative, capped, roll_up};
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
            }),
            representative: Representative::Read(Some(id)),
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
            .map(|album| match album.scored.candidate {
                Candidate::Release(id) | Candidate::Master(id) => id,
            })
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
    fn an_artist_scores_its_best_album_with_a_quarter_and_a_tenth_of_the_next() {
        let albums = vec![
            album(10, &[1], 1.0),
            album(11, &[2], 0.9),
            album(12, &[1], 0.5),
            album(13, &[1], 0.4),
            // A fourth counts nothing.
            album(14, &[1], 0.3),
            album(15, &[3], 0.2),
        ];
        let artists = roll_up(&mut ranked(albums), &|id, _| id == 3, 10).unwrap();
        let ids: Vec<u64> = artists.iter().map(|artist| artist.id).collect();
        assert_eq!(ids, [1, 2]);
        let first = 1.0 + 0.25 * 0.5 + 0.1 * 0.4;
        assert!((artists[0].score - 1.0).abs() < 1e-12);
        assert!((artists[1].score - 0.9 / first).abs() < 1e-12);
        assert_eq!(artists[0].albums, 3);
    }

    #[test]
    fn the_roll_up_stops_where_the_list_and_its_order_are_settled() {
        // Artist 1 at 1.0, artist 2 at 0.5, then artists of 0.1: no one can pass artist 2
        // with the albums left, and artist 1's album of 0.1 at the end isn't read.
        let mut albums = vec![album(1, &[1], 1.0), album(2, &[2], 0.5)];
        albums.extend((3..100).map(|id| album(id, &[id], 0.1)));
        albums.push(album(100, &[1], 0.1));
        let artists = roll_up(&mut ranked(albums), &|_, _| false, 2).unwrap();
        assert_eq!(artists[0].id, 1);
        assert_eq!(artists[0].albums, 1);
        assert_eq!(artists[1].id, 2);
    }

    #[test]
    fn an_artist_read_below_the_list_can_climb_into_it() {
        // Artist 3 starts at 0.79, under artist 2's 0.8. After 61 artists of 0.5, which a
        // check at album 64 would take for the end of what can matter, it gets two albums
        // of 0.5 more and passes artist 2.
        let mut albums = vec![
            album(1, &[1], 1.0),
            album(2, &[2], 0.8),
            album(3, &[3], 0.79),
        ];
        albums.extend((4..65).map(|id| album(id, &[id], 0.5)));
        albums.push(album(100, &[3], 0.5));
        albums.push(album(101, &[3], 0.5));
        let artists = roll_up(&mut ranked(albums), &|_, _| false, 2).unwrap();
        let ids: Vec<u64> = artists.iter().map(|artist| artist.id).collect();
        assert_eq!(ids, [1, 3]);
    }

    #[test]
    fn a_limit_of_0_lists_nothing() {
        let albums = || (10..80).map(|id| album(id, &[id], 1.0)).collect();
        assert_eq!(kept(albums(), &HashMap::new(), 0), Vec::<u64>::new());
        let artists = roll_up(&mut ranked(albums()), &|_, _| false, 0).unwrap();
        assert_eq!(artists.len(), 0, "{artists:?}");
    }
}
