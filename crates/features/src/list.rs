//! The lists of recommendations (recommendations.md, Lists and evaluation; ADRs 0026, 0028):
//! the whole library's, read by rank from the stored list, or a seed album's or a seed
//! artist's, computed when first asked for and kept on disk; all of a scope's albums, or only
//! the missing albums of its artists, kept by style, year and label; at most `CAP` albums of
//! one artist or one label; every release and master the feedback marks left out; a page at
//! a time. And the artists of the albums, each by its best three, the library's own artists
//! left out, since they are suggested only through albums of theirs the library doesn't
//! have. `recommend` prints through [`List`], and `POST /v1/recommendations` answers
//! through it.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};

use catalog::community::LockError;
use catalog::discogs::{Catalog, CatalogError, Reading};
use catalog::error::DatabaseError;
use catalog::feedback::Kind;
pub use catalog::score::Filter;
use catalog::score::{self as stored, Computed, Head, Inputs, Scores};
use catalog::seed_list::{self, Seed};
use config::Config;
use primitives::fold;

use crate::artist::{Artists, artist_keys};
use crate::clock::{Clock, unix_seconds};
use crate::connection::Connections;
use crate::feedback::{self, FeedbackError};
use crate::library::Library;
use crate::profile::{self, Album as LibraryAlbum, Profile, ProfileError};
use crate::reason;
use crate::rescore::{self, Listed, RescoreError};
use crate::score::{self, Named};
use crate::seed::Seeds;

/// How many albums of one artist, and of one label, a list shows.
const CAP: usize = 3;
/// How many seed lists are kept on disk (ADR 0028).
const SEED_LISTS: usize = 32;
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

/// A list asked for on the command line.
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
    pub albums: Vec<Listed>,
    /// Its artists, best first; `None` for the missing albums, whose artists are all the
    /// library's.
    pub artists: Option<Vec<Artist>>,
}

/// An artist of a list's albums.
#[derive(Debug, Clone)]
pub struct Artist {
    /// Its place in the artists, from 1 for the best.
    pub rank: u64,
    /// Its Discogs id.
    pub id: u64,
    /// Its name, `None` when the catalog has no row for it.
    pub name: Option<String>,
    /// Its best album's score, with a quarter of its second's and a tenth of its third's,
    /// from 0 to 1 for the best artist.
    pub score: f64,
    /// The albums its score counts, at most 3.
    pub albums: u64,
    /// Its best album.
    pub best: Listed,
}

/// A page of a list.
#[derive(Debug)]
pub struct Page<T> {
    /// The albums or artists on it, best first.
    pub items: Vec<T>,
    /// Whether the list goes on past it.
    pub more: bool,
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
    /// A stored or kept list can't be read or written.
    Rescore(RescoreError),
    /// The feedback can't be read.
    Feedback(FeedbackError),
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
            Self::Feedback(error) => error.fmt(f),
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

impl From<DatabaseError> for ListError {
    fn from(error: DatabaseError) -> Self {
        Self::Rescore(RescoreError::Database(error))
    }
}

impl From<LockError> for ListError {
    fn from(error: LockError) -> Self {
        Self::Rescore(RescoreError::from(error))
    }
}

impl From<FeedbackError> for ListError {
    fn from(error: FeedbackError) -> Self {
        Self::Feedback(error)
    }
}

/// The lists `ask` asks for, as `recommend` prints them: its first `limit` albums and,
/// unless only the missing ones, its first `limit` artists. A seed list is computed on
/// `clock` when none is kept, saying so on `log`.
///
/// # Errors
///
/// As for [`List::open`] and [`List::albums`].
pub fn lists(
    config: &Config,
    clock: &dyn Clock,
    ask: &Ask,
    log: &mut dyn FnMut(&str),
) -> Result<Lists, ListError> {
    let list = List::open(config, clock, &ask.of, log)?;
    let albums = list
        .albums(&Filter::default(), ask.missing, 0, ask.limit)?
        .items;
    let artists = (!ask.missing)
        .then(|| list.artists(0, ask.limit))
        .transpose()?
        .map(|page| page.items);
    let heading = if ask.missing {
        format!("{}\nonly the missing albums of its artists", list.heading)
    } else {
        list.heading
    };
    Ok(Lists {
        heading,
        albums,
        artists,
    })
}

/// A list opened to read pages of: the stored list of the whole library, or a seed's kept
/// one. Every page reads the one file opened, so a rebuild renamed in meanwhile doesn't mix
/// two lists.
pub struct List {
    data_dir: PathBuf,
    /// What the list is of, for the command line.
    heading: String,
    scores: Scores,
    computed: Computed,
}

impl List {
    /// The list `of` some seeds, from the catalog, the kept profile and the data directory.
    /// The whole library's is the stored one. A seed's is the one kept on disk when it was
    /// computed from the current catalog and profile; otherwise it is computed on `clock`
    /// from one pass over the catalog and kept. One computation runs at a time across
    /// processes: another waits for it, saying so on `log`.
    ///
    /// # Errors
    ///
    /// When there is no catalog or kept profile; for the whole library, when no list is
    /// stored or it was computed from another catalog or profile; when the album or artist
    /// asked for has no seed; and when a read or a write fails.
    pub fn open(
        config: &Config,
        clock: &dyn Clock,
        of: &Of,
        log: &mut dyn FnMut(&str),
    ) -> Result<Self, ListError> {
        let data_dir = &config.data_dir;
        let catalog = match Catalog::open_read_only(data_dir)? {
            Reading::Missing => return Err(ListError::NoCatalog),
            Reading::OtherShape => return Err(ListError::OtherCatalog),
            Reading::Open(catalog) => catalog,
        };
        let profile = profile::kept(data_dir)?.ok_or(ListError::NoProfile)?;
        let (heading, (scores, computed)) = match of {
            Of::Library => {
                let Reading::Open(scores) = Scores::open_read_only(data_dir)? else {
                    return Err(ListError::NoList);
                };
                let computed = scores.computed()?;
                if computed.inputs != rescore::inputs(&catalog, &profile)? {
                    let rebuilding = stored::running(data_dir)?;
                    return Err(ListError::Stale { rebuilding });
                }
                let heading = format!(
                    "the whole library: the list computed from the set of {} and {} seeds, {} \
                     candidates",
                    computed.inputs.catalog_set, computed.seeds, computed.candidates
                );
                (heading, (scores, computed))
            }
            Of::Album(release_id) => {
                let albums: Vec<LibraryAlbum> = profile
                    .albums
                    .iter()
                    .filter(|album| album.discogs_release_id == Some(*release_id))
                    .cloned()
                    .collect();
                let Some(first) = albums.first() else {
                    return Err(ListError::NotInProfile(*release_id));
                };
                let heading = format!(
                    "the seed album {release_id}: {} – {}",
                    first.artist, first.title
                );
                let seed = SeedList {
                    seed: Seed::Album(*release_id),
                    albums: &albums,
                    catalog: &catalog,
                    profile: &profile,
                };
                let kept = seed.kept_or_computed(data_dir, clock, log, || {
                    ListError::NotInCatalog(*release_id)
                })?;
                (heading, kept)
            }
            Of::Artist(name) => {
                let key = fold::key(name, false);
                let albums: Vec<LibraryAlbum> = profile
                    .albums
                    .iter()
                    .filter(|album| artist_keys(&album.artist).any(|artist| artist == key))
                    .cloned()
                    .collect();
                if albums.is_empty() {
                    return Err(ListError::NoAlbumBy(name.clone()));
                }
                let seed = SeedList {
                    seed: Seed::Artist(&key),
                    albums: &albums,
                    catalog: &catalog,
                    profile: &profile,
                };
                let kept = seed
                    .kept_or_computed(data_dir, clock, log, || ListError::NoSeedBy(name.clone()))?;
                let seeds = kept.1.seeds;
                let heading = format!(
                    "the {seeds} seed album{} by {name}",
                    if seeds == 1 { "" } else { "s" }
                );
                (heading, kept)
            }
        };
        Ok(Self {
            data_dir: data_dir.clone(),
            heading,
            scores,
            computed,
        })
    }

    /// What the list was computed from and what came of it.
    #[must_use]
    pub fn computed(&self) -> &Computed {
        &self.computed
    }

    /// Up to `limit` albums after the first `offset` of the list as it is shown: the
    /// candidates `filter` keeps, best first, only those by an artist of the scope's albums
    /// when `missing`, without the releases and masters the feedback marks now, and at most
    /// `CAP` of one artist or one label. A candidate is marked when its own kind and id is,
    /// or when its representative release is.
    ///
    /// # Errors
    ///
    /// When the list or the feedback can't be read.
    pub fn albums(
        &self,
        filter: &Filter,
        missing: bool,
        offset: usize,
        limit: usize,
    ) -> Result<Page<Listed>, ListError> {
        let marked = feedback::marked(&self.data_dir)?;
        let artists = missing
            .then(|| self.scores.scope())
            .transpose()?
            .map(|(ids, names)| Artists::from_parts(ids, names));
        let mut cap = Cap::default();
        let (mut skipped, mut ranks, mut more) = (0, Vec::new(), false);
        self.scores.walk(filter, &mut |head| {
            let is_marked = marked.contains(&(head.kind, head.id))
                || head
                    .representative
                    .is_some_and(|id| marked.contains(&(Kind::Release, id)));
            let kept = !is_marked
                && artists
                    .as_ref()
                    .is_none_or(|artists| artists.has_any(&head.artists));
            if !kept || !cap.admits(&head) {
                return true;
            }
            if skipped < offset {
                skipped += 1;
                return true;
            }
            if ranks.len() == limit {
                more = true;
                return false;
            }
            ranks.push(head.rank);
            true
        })?;
        let items = self
            .scores
            .rows(&ranks)?
            .into_iter()
            .zip(ranks)
            .map(|(row, rank)| rescore::listed(rank, row))
            .collect::<Result<_, _>>()?;
        Ok(Page { items, more })
    }

    /// Up to `limit` artists after the first `offset` of the list's roll-up, best first,
    /// each with its best album. The roll-up was made from every album of the list when it
    /// was computed, so no filter applies to it.
    ///
    /// # Errors
    ///
    /// When the list can't be read.
    pub fn artists(&self, offset: usize, limit: usize) -> Result<Page<Artist>, ListError> {
        let mut rows = self
            .scores
            .artists(offset as u64, (limit as u64).saturating_add(1))?;
        let more = rows.len() > limit;
        rows.truncate(limit);
        let items = rescore::best_of(&self.scores, rows)?
            .into_iter()
            .zip(offset as u64 + 1..)
            .map(|((artist, best), rank)| Artist {
                rank,
                id: artist.id,
                name: artist.name,
                score: artist.score,
                albums: artist.albums,
                best,
            })
            .collect();
        Ok(Page { items, more })
    }
}

/// A seed list to find kept or to compute: the seeds `albums` of the kept `profile` are
/// tagged with.
struct SeedList<'a> {
    seed: Seed<'a>,
    albums: &'a [LibraryAlbum],
    catalog: &'a Catalog,
    profile: &'a Profile,
}

impl SeedList<'_> {
    /// The list kept for the seed when it is of the current inputs; otherwise computed and
    /// kept, under the seed lists' lock. `none` is the error when no seed is among the
    /// albums.
    fn kept_or_computed(
        &self,
        data_dir: &Path,
        clock: &dyn Clock,
        log: &mut dyn FnMut(&str),
        none: impl FnOnce() -> ListError,
    ) -> Result<(Scores, Computed), ListError> {
        let inputs = rescore::inputs(self.catalog, self.profile)?;
        if let Some(kept) = self.kept(data_dir, &inputs, clock, log)? {
            return Ok(kept);
        }
        let _lock = seed_list::lock(data_dir, || {
            log("seed list: waiting for the one being computed");
        })?;
        // The one this waited for may have been this seed's.
        if let Some(kept) = self.kept(data_dir, &inputs, clock, log)? {
            return Ok(kept);
        }
        log("seed list: computing it from a pass over the catalog");
        // The seeds are read for these albums alone, so the pass keeps only what connects to
        // them and their weights are scaled among themselves (ADR 0026).
        let their_albums = Profile {
            albums: self.albums.to_vec(),
        };
        let seeds = Seeds::read(&their_albums, self.catalog)?;
        if seeds.seeds.is_empty() {
            return Err(none());
        }
        let connections = Connections::read(self.catalog, &seeds)?;
        // The whole profile's albums are still the library's, and never suggested.
        let library = Library::new(self.profile, self.catalog)?;
        let scored = score::score(&seeds, connections, &library)
            .map_err(|error| self.catalog.inconsistent(error.to_string()))?;
        let library_artists = Artists::read(self.profile.albums.iter(), self.catalog)?;
        // A seed's missing albums are other albums of its own albums' artists.
        let scope = Artists::read(self.albums.iter(), self.catalog)?;
        let mut writer = seed_list::writer(data_dir, &self.seed)?;
        rescore::write(&mut writer, &scored, self.catalog, &library_artists, &scope)?;
        let computed = writer
            .finish(&inputs, seeds.seeds.len() as u64, unix_seconds(clock.now()))
            .map_err(RescoreError::from)?;
        seed_list::asked(data_dir, &self.seed, clock.now())?;
        seed_list::sweep(data_dir, SEED_LISTS, &inputs)?;
        match seed_list::open(data_dir, &self.seed)? {
            Reading::Open(list) => Ok((list, computed)),
            Reading::Missing | Reading::OtherShape => {
                Err(RescoreError::Reason("no seed list where one was just written").into())
            }
        }
    }

    /// The list kept for the seed, if it was computed from `inputs`, marked as asked for now.
    /// One that can't be read is said on `log` and counts as none, to be computed again.
    fn kept(
        &self,
        data_dir: &Path,
        inputs: &Inputs,
        clock: &dyn Clock,
        log: &mut dyn FnMut(&str),
    ) -> Result<Option<(Scores, Computed)>, ListError> {
        let Reading::Open(scores) = seed_list::open(data_dir, &self.seed)? else {
            return Ok(None);
        };
        match scores.computed() {
            Ok(computed) if computed.inputs == *inputs => {
                seed_list::asked(data_dir, &self.seed, clock.now())?;
                Ok(Some((scores, computed)))
            }
            Ok(_) => Ok(None),
            Err(error) => {
                log(&format!(
                    "seed list: the one kept can't be read ({error}); computing it again"
                ));
                Ok(None)
            }
        }
    }
}

/// The counts of the cap: how many albums of each artist and each label a list shows.
#[derive(Default)]
struct Cap {
    by_artist: HashMap<u64, usize>,
    by_label: HashMap<u64, usize>,
}

impl Cap {
    /// Whether `head` fits, none of its artists nor its label having `CAP` albums in the
    /// list already; one that fits is counted. An album of several artists counts for each.
    fn admits(&mut self, head: &Head) -> bool {
        let artists: HashSet<u64> = head.artists.iter().map(|artist| artist.id).collect();
        let full =
            |counts: &HashMap<u64, usize>, id: &u64| counts.get(id).is_some_and(|&n| n >= CAP);
        if artists.iter().any(|id| full(&self.by_artist, id))
            || head.label_id.is_some_and(|id| full(&self.by_label, &id))
        {
            return false;
        }
        for id in artists {
            *self.by_artist.entry(id).or_default() += 1;
        }
        if let Some(id) = head.label_id {
            *self.by_label.entry(id).or_default() += 1;
        }
        true
    }
}

/// The words of a candidate's strongest reason, if it has one.
fn strongest(listed: &Listed) -> Option<String> {
    listed
        .scored
        .reasons
        .first()
        .map(|reason| reason::words(&reason.connection))
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
            let artists: Vec<String> = album
                .artists
                .iter()
                .map(|Named { id, name }| name.clone().unwrap_or_else(|| format!("artist {id}")))
                .collect();
            let year = album
                .year
                .map_or_else(String::new, |year| format!(" ({year})"));
            writeln!(
                f,
                "{:>3}. {:.2}  {} – {}{year}, {} {id}",
                rank + 1,
                album.scored.score,
                artists.join(" / "),
                album.title,
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
        for artist in artists {
            let name = artist
                .name
                .clone()
                .unwrap_or_else(|| format!("artist {}", artist.id));
            writeln!(
                f,
                "{:>3}. {:.2}  {name}, artist {}, from {} album{}",
                artist.rank,
                artist.score,
                artist.id,
                artist.albums,
                if artist.albums == 1 { "" } else { "s" }
            )?;
            let title = &artist.best.title;
            match strongest(&artist.best) {
                Some(reason) => writeln!(f, "        best: {title}; {reason}")?,
                None => writeln!(f, "        best: {title}")?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use catalog::feedback::Kind;
    use catalog::score::Head;

    use super::{CAP, Cap};
    use crate::score::Named;

    /// A release `id` by `artists` on `label`.
    fn head(id: u64, artists: &[u64], label: Option<u64>) -> Head {
        Head {
            rank: id,
            kind: Kind::Release,
            id,
            representative: Some(id),
            label_id: label,
            artists: artists.iter().map(|&id| Named { id, name: None }).collect(),
        }
    }

    /// The releases the cap admits of `heads`, in order.
    fn admitted(heads: &[Head]) -> Vec<u64> {
        let mut cap = Cap::default();
        heads
            .iter()
            .filter(|head| cap.admits(head))
            .map(|head| head.id)
            .collect()
    }

    #[test]
    fn one_artist_takes_at_most_cap_albums() {
        // Five by artist 1, one of them with artist 2, then one by artist 2.
        let heads = [
            head(10, &[1], None),
            head(11, &[1], None),
            head(12, &[1, 2], None),
            head(13, &[1], None),
            head(14, &[1], None),
            head(15, &[2], None),
        ];
        assert_eq!(CAP, 3);
        assert_eq!(admitted(&heads), [10, 11, 12, 15]);
    }

    #[test]
    fn one_label_takes_at_most_cap_albums() {
        let heads: Vec<Head> = (10..15).map(|id| head(id, &[id], Some(7))).collect();
        assert_eq!(admitted(&heads), [10, 11, 12]);
        // No label, no cap.
        let heads: Vec<Head> = (10..15).map(|id| head(id, &[id], None)).collect();
        assert_eq!(admitted(&heads), [10, 11, 12, 13, 14]);
    }

    #[test]
    fn an_album_the_cap_skips_counts_for_none_of_its_artists() {
        // 13 is skipped for artist 1, so artist 2 still has room for three.
        let heads = [
            head(10, &[1], None),
            head(11, &[1], None),
            head(12, &[1], None),
            head(13, &[1, 2], None),
            head(14, &[2], None),
            head(15, &[2], None),
            head(16, &[2], None),
        ];
        assert_eq!(admitted(&heads), [10, 11, 12, 14, 15, 16]);
    }
}
