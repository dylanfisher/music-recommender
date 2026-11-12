//! The scores' rebuild (ADR 0026): the whole library's list scored from the catalog and the
//! kept profile and written to `scores.sqlite`, after an import swaps a catalog in and after
//! a profile is kept, and at `serve`'s start to catch up on one missed. The list records
//! what it was computed from, the catalog's set and swap time, what the score reads from
//! the profile and the version of the scoring code, and a rebuild that finds all unchanged
//! writes nothing.
//!
//! One rebuild runs at a time in the data directory, whichever process asks: each waits for
//! `scores.lock`. In `serve` the rebuilds run on one thread of their own, minutes of work
//! and gigabytes of memory, on a thread of its own; requests that arrive while one runs make one
//! more run after it, not one each. A seed list's computation (`write` too, for one seed's
//! candidates) runs on the request's thread that asked for it (ADR 0028).

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

use catalog::community::{Community, LockError};
use catalog::discogs::{Catalog, CatalogError, Entry, Reading};
use catalog::error::DatabaseError;
use catalog::score::{
    self as stored, ArtistRow, Computed, FinishError, Inputs, Item, ReasonKind, Row, Scores, Writer,
};
use catalog::swap::SwapError;
use config::Config;

use crate::artist::{self, Artists};
use crate::candidate::Candidate;
use crate::clock::{Clock, unix_seconds};
use crate::community_factor;
use crate::connection::Connections;
use crate::fetch::RETRY_AFTER_FAILURE;
use crate::library::Library;
use crate::profile::{self, Album as LibraryAlbum, Profile, ProfileError};
use crate::score::{self, Connection, Named, Reason, Scored};
use crate::seed::{Seeds, is_not_on_label, read_label};

/// The longest the scores' thread waits to retry a rebuild that fails again and again.
const RETRY_CAP: Duration = Duration::from_hours(24);

/// What a rebuild did.
#[derive(Debug)]
pub enum Summary {
    /// There is no catalog to score from.
    NoCatalog,
    /// The catalog's tables are another version's, so it can't be read.
    OtherCatalog,
    /// No profile has been kept, so there are no seeds.
    NoProfile,
    /// The list in use is from this catalog and profile already.
    Current(Computed),
    /// A list was computed and swapped in.
    Built(Computed),
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let list = |computed: &Computed| {
            format!(
                "the set of {} and the kept profile: {} candidates from {} seeds",
                computed.inputs.catalog_set, computed.candidates, computed.seeds
            )
        };
        match self {
            Self::NoCatalog => {
                f.write_str("scores: no catalog yet, so nothing to score; an import builds one")
            }
            Self::OtherCatalog => f.write_str(
                "scores: the catalog's tables are from another version, so nothing to score \
                 until the next import replaces it",
            ),
            Self::NoProfile => f.write_str(
                "scores: no profile kept yet, so nothing to score; POST /v1/profile or \
                 `profile load` keeps one",
            ),
            Self::Current(computed) => write!(f, "scores: already from {}", list(computed)),
            Self::Built(computed) => write!(f, "scores: computed from {}", list(computed)),
        }
    }
}

/// Why a rebuild couldn't run or stopped. The list in use stays as it was.
#[derive(Debug)]
pub enum RescoreError {
    /// The rebuild's lock file can't be opened or locked.
    Lock(PathBuf, std::io::Error),
    /// The catalog failed.
    Catalog(CatalogError),
    /// The catalog has no set or no swap time, which every import records.
    Unswapped,
    /// The kept profile can't be read.
    Profile(ProfileError),
    /// The scores database failed.
    Database(DatabaseError),
    /// The new list couldn't be swapped in, or it is in use but syncing the data directory
    /// after the rename failed ([`SwapError::is_swapped`]).
    Swap(SwapError),
    /// The scores database holds a reason without what its kind needs.
    Reason(&'static str),
}

impl fmt::Display for RescoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lock(path, error) => write!(f, "{}: {error}", path.display()),
            Self::Catalog(error) => error.fmt(f),
            Self::Unswapped => {
                f.write_str("the catalog records no set or no swap time, which every import writes")
            }
            Self::Profile(error) => error.fmt(f),
            Self::Database(error) => error.fmt(f),
            Self::Swap(error) => error.fmt(f),
            Self::Reason(problem) => write!(f, "the scores hold {problem}"),
        }
    }
}

impl std::error::Error for RescoreError {}

impl RescoreError {
    /// Whether the list was built and is in use despite this error, which a rebuild returns
    /// when only the sync of the data directory after the rename failed.
    #[must_use]
    pub fn is_built(&self) -> bool {
        matches!(self, Self::Swap(error) if error.is_swapped())
    }
}

impl From<LockError> for RescoreError {
    fn from(error: LockError) -> Self {
        match error {
            // `stored::lock` waits rather than refusing, so it never says busy.
            LockError::Busy(path) => {
                Self::Lock(path, std::io::Error::other("held by another process"))
            }
            LockError::Io(path, error) => Self::Lock(path, error),
        }
    }
}

impl From<CatalogError> for RescoreError {
    fn from(error: CatalogError) -> Self {
        Self::Catalog(error)
    }
}

impl From<ProfileError> for RescoreError {
    fn from(error: ProfileError) -> Self {
        Self::Profile(error)
    }
}

impl From<FinishError> for RescoreError {
    fn from(error: FinishError) -> Self {
        match error {
            FinishError::Failed(error) => Self::Database(error),
            FinishError::Swap(error) => Self::Swap(error),
        }
    }
}

impl From<DatabaseError> for RescoreError {
    fn from(error: DatabaseError) -> Self {
        Self::Database(error)
    }
}

/// Rebuilds the list in `config`'s data directory if the catalog or what the score reads
/// from the kept profile changed since it was computed, or there is none. Waits for a
/// rebuild another process is running, saying so on `log`, and then compares.
///
/// # Errors
///
/// When the lock, the catalog, the kept profile or the scores database fails. When only the
/// sync of the data directory after the new list's rename fails, the list is built and in
/// use, and the error says so ([`RescoreError::is_built`]).
pub fn run(
    config: &Config,
    clock: &dyn Clock,
    log: &mut dyn FnMut(&str),
) -> Result<Summary, RescoreError> {
    let data_dir = &config.data_dir;
    let _lock = stored::lock(data_dir, || {
        log("scores: waiting for the rebuild another process is running");
    })?;
    let catalog = match Catalog::open_read_only(data_dir)? {
        Reading::Missing => return Ok(Summary::NoCatalog),
        Reading::OtherShape => return Ok(Summary::OtherCatalog),
        Reading::Open(catalog) => catalog,
    };
    let Some(profile) = profile::kept(data_dir)? else {
        return Ok(Summary::NoProfile);
    };
    let inputs = inputs(&catalog, &profile, config.community_weight)?;
    // A list that can't be read is no usable list: rebuilding replaces it, where failing
    // here would fail every rebuild after it for good.
    match stored_computed(data_dir) {
        Ok(Some(computed)) if computed.inputs == inputs => return Ok(Summary::Current(computed)),
        Ok(_) => {}
        Err(error) => log(&format!(
            "scores: the list in use can't be read ({error}); computing a new one over it"
        )),
    }
    let seeds = Seeds::read(&profile, &catalog)?;
    // The whole library's missing albums are by the library's artists.
    let computed = compute(
        &catalog,
        &profile,
        seeds,
        None,
        &inputs,
        clock,
        data_dir,
        || Writer::create(data_dir),
    )?;
    Ok(Summary::Built(computed))
}

/// The pass over the catalog that scores the candidates for `seeds` and writes them to the
/// list `create` makes, giving what the list was computed from. The seeds are those of the
/// whole `profile` or of some of its albums, a seed list's; the albums of `profile` are the
/// library's either way, and never suggested. The list's missing albums are other albums of
/// the artists of `scope`'s albums, or of the library's when `None`. A master's
/// representative is its most-owned version when the community database in `data_dir` has
/// its versions, and each candidate's score is weighed by its representative's community
/// values there at the inputs' weight (ADR 0032).
///
/// # Errors
///
/// When the catalog, the writer or the score fails.
#[expect(
    clippy::too_many_arguments,
    reason = "the whole library's list and a seed list differ in each"
)]
pub(crate) fn compute(
    catalog: &Catalog,
    profile: &Profile,
    seeds: Seeds,
    scope: Option<&[LibraryAlbum]>,
    inputs: &Inputs,
    clock: &dyn Clock,
    data_dir: &Path,
    create: impl FnOnce() -> Result<Writer, DatabaseError>,
) -> Result<Computed, RescoreError> {
    // A database of another shape holds nothing this build reads; the sync starts it again.
    let community = match Community::open_read_only(data_dir)? {
        Reading::Open(db) => Some(db),
        Reading::Missing | Reading::OtherShape => None,
    };
    // The big intermediates are dropped before the writer finishes, which syncs and renames
    // the file, and before a seed list's sweep.
    let (writer, seed_count) = {
        let connections = Connections::read(catalog, &seeds)?;
        let library = Library::new(profile, catalog)?;
        let mut scored = score::score(&seeds, connections, &library)
            .map_err(|error| catalog.inconsistent(error.to_string()))?;
        let representatives = community_factor::representatives(&scored, community.as_ref())?;
        let values = community_factor::values(&representatives, community.as_ref())?;
        community_factor::weigh(&mut scored, &values, inputs.community_weight)
            .map_err(|error| catalog.inconsistent(error.to_string()))?;
        let mut writer = create()?;
        let library_artists = Artists::read(profile.albums.iter(), catalog)?;
        let scope = match scope {
            Some(albums) => &Artists::read(albums.iter(), catalog)?,
            None => &library_artists,
        };
        write(
            &mut writer,
            &scored,
            &representatives,
            catalog,
            &library_artists,
            scope,
        )?;
        let seed_count = seeds.seeds.len() as u64;
        drop(seeds);
        (writer, seed_count)
    };
    Ok(writer.finish(inputs, seed_count, unix_seconds(clock.now()))?)
}

/// Adds `scored`, best first, to `writer`, each with its representative of
/// `representatives`, what its row shows and the filters read, then the artists they roll up to, leaving out `library_artists`, and `scope`, the
/// artists whose other albums are the list's missing albums. The whole library's list and a
/// seed list are written by this alike, so reading either never reads the catalog or rolls
/// up.
fn write(
    writer: &mut Writer,
    scored: &[(Scored, Entry)],
    representatives: &HashMap<Candidate, Option<u64>>,
    catalog: &Catalog,
    library_artists: &Artists,
    scope: &Artists,
) -> Result<(), RescoreError> {
    let style_names = catalog.style_names()?;
    let mut labels = HashMap::new();
    for (candidate, entry) in scored {
        // `representatives` was read for every candidate of this list.
        let representative = representatives.get(&candidate.candidate).copied().flatten();
        let label = representative
            .map(|id| catalog.first_label(id))
            .transpose()?
            .flatten()
            .map(|id| label(catalog, &mut labels, id))
            .transpose()?
            .flatten();
        let styles = entry
            .styles
            .iter()
            .map(|&id| Named::new(u64::from(id), style_names.get(&id).cloned()))
            .collect();
        writer.add(&row(candidate, entry, representative, label, styles))?;
    }
    let rolled = artist::roll_up(
        scored
            .iter()
            .map(|(candidate, entry)| Some((entry, candidate.score))),
        &|id, name| library_artists.has(id, name),
    );
    for artist in &rolled {
        writer.add_artist(&stored::ArtistRow {
            id: artist.id,
            name: artist.name.clone(),
            score: artist.score,
            albums: artist.albums as u64,
            best_rank: artist.best as u64 + 1,
        })?;
    }
    let (ids, names) = scope.parts();
    writer.add_scope(ids.iter().copied(), names.iter().map(String::as_str))?;
    Ok(())
}

/// The label `id` as a row shows it, read once into `labels`: `None` for Discogs's
/// placeholder for none, and without a name when the catalog has no row for it.
fn label(
    catalog: &Catalog,
    labels: &mut HashMap<u64, Option<(String, Option<u64>)>>,
    id: u64,
) -> Result<Option<Named>, CatalogError> {
    Ok(match read_label(catalog, labels, id)? {
        Some((name, _)) if is_not_on_label(&name) => None,
        Some((name, _)) => Some(Named::new(id, Some(name))),
        None => Some(Named::new(id, None)),
    })
}

/// What the list in use was computed from, `None` when there is none or one of another shape.
fn stored_computed(data_dir: &Path) -> Result<Option<Computed>, DatabaseError> {
    match Scores::open_read_only(data_dir)? {
        Reading::Open(scores) => scores.computed().map(Some),
        Reading::Missing | Reading::OtherShape => Ok(None),
    }
}

/// What a list computed now from `catalog` and `profile`, with the community values at
/// `community_weight`, is computed from.
///
/// # Errors
///
/// When the catalog can't be read or has never been swapped in.
pub(crate) fn inputs(
    catalog: &Catalog,
    profile: &Profile,
    community_weight: f64,
) -> Result<Inputs, RescoreError> {
    let summary = catalog.summary()?;
    let (Some(set), Some(swapped_at)) = (summary.imported.first(), summary.swapped_at) else {
        return Err(RescoreError::Unswapped);
    };
    Ok(Inputs {
        catalog_set: set.date.clone(),
        catalog_swapped_at: swapped_at,
        profile_digest: stored::digest(&profile_inputs(profile)),
        scoring_version: score::VERSION,
        community_weight,
    })
}

// A change to what `profile_inputs` reads or to the rows `row` and `stored_reason` write
// that changes a stored list needs a bump of `score::VERSION`.
/// What the score reads from `profile`: each album's release id, plays, loves and rating,
/// which make the seeds and their weights, and its artist and title, which the exclusions
/// match. One JSON array for each album, sorted, so the same albums in another order are the
/// same inputs. Its `key`, `last_played` and `skips` are Rediscover's alone (ADR 0034), so a
/// profile that changes only them rebuilds nothing.
fn profile_inputs(profile: &Profile) -> String {
    let mut albums: Vec<String> = profile
        .albums
        .iter()
        .map(|album| {
            serde_json::json!([
                album.discogs_release_id,
                album.plays,
                album.loved,
                album.rating,
                album.artist,
                album.title
            ])
            .to_string()
        })
        .collect();
    albums.sort_unstable();
    albums.join("\n")
}

/// A candidate of a stored list, with what its row shows.
#[derive(Debug, Clone)]
pub struct Listed {
    /// Its rank in the list, from 1 for the best.
    pub rank: u64,
    /// The candidate, its score and its reasons.
    pub scored: Scored,
    /// The release whose community data stands for it, `None` when the catalog had no row
    /// for its master.
    pub representative: Option<u64>,
    /// Its title.
    pub title: String,
    /// Its year, when it has one.
    pub year: Option<u16>,
    /// Its artists in order.
    pub artists: Vec<Named>,
    /// Its representative's first label, `None` with none or a "Not On Label".
    pub label: Option<Named>,
    /// Its styles.
    pub styles: Vec<Named>,
}

/// Each of `artists` of `scores` with its best candidate.
///
/// # Errors
///
/// When the read fails, or an artist's best candidate is missing from the list.
pub(crate) fn best_of(
    scores: &Scores,
    artists: Vec<ArtistRow>,
) -> Result<Vec<(ArtistRow, Listed)>, RescoreError> {
    artists
        .into_iter()
        .map(|artist| {
            let best = scores
                .candidate(artist.best_rank)?
                .ok_or(RescoreError::Reason(
                    "an artist whose best candidate isn't listed",
                ))?;
            let best = listed(artist.best_rank, best)?;
            Ok((artist, best))
        })
        .collect()
}

/// The requests for a rebuild that `serve`'s scores thread waits on: the dumps' thread
/// after a swap and the API after a profile make them. Clones share the requests.
#[derive(Clone, Default)]
pub struct Requests(Arc<Shared>);

/// What clones of `Requests` share.
#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

/// The requests and the retry waits, under one lock so that a request can't land between
/// a reading of one field and a change to another.
#[derive(Default)]
struct State {
    /// Whether a rebuild is asked for and hasn't started.
    pending: bool,
    /// How many requests were made. A retry asks again on waking only if none was made
    /// since the rebuild it retries took its request: a request starts the waits over, and
    /// a sleeper of the old waits would ask for a rebuild nothing calls for.
    generation: u64,
    /// How many rebuilds in a row failed since the last one that worked or request made.
    failures: u32,
}

impl State {
    /// Counts a rebuild that failed and says how long to wait before the retry: an hour
    /// after the first of a run, twice as long after each next, at most `RETRY_CAP`. A
    /// rebuild that fails for a reason in the data fails the same way each time, and each
    /// try is a full pass over the catalog.
    fn failed(&mut self) -> Duration {
        let doubled = 1u32.checked_shl(self.failures).unwrap_or(u32::MAX);
        self.failures = self.failures.saturating_add(1);
        RETRY_AFTER_FAILURE.saturating_mul(doubled).min(RETRY_CAP)
    }
}

impl Requests {
    /// Asks for a rebuild. Asked again before it starts, it is still one. A new request
    /// starts the waits between retries over, since what it asks about is new.
    pub fn request(&self) {
        let mut state = lock(&self.0.state);
        state.generation += 1;
        state.failures = 0;
        state.pending = true;
        drop(state);
        self.0.wake.notify_one();
    }

    /// Whether a rebuild is asked for and hasn't started.
    #[must_use]
    pub fn pending(&self) -> bool {
        lock(&self.0.state).pending
    }

    /// Waits until a rebuild is asked for, takes the request, and returns the generation
    /// it was taken under.
    fn take(&self) -> u64 {
        let mut state = self
            .0
            .wake
            .wait_while(lock(&self.0.state), |state| !state.pending)
            .unwrap_or_else(PoisonError::into_inner);
        state.pending = false;
        state.generation
    }

    /// Counts the failure of the rebuild taken under `generation` and asks again once
    /// `clock` has waited the retry wait, from a thread of its own so that requests that
    /// arrive meanwhile are served at once. The thread does nothing if a request was made
    /// while it waited, which made its wait stale. Does nothing when a request was made
    /// since `generation`: that request starts the waits over and runs next.
    fn retry_later(
        &self,
        clock: &Arc<dyn Clock>,
        generation: u64,
    ) -> Option<thread::JoinHandle<()>> {
        let wait = {
            let mut state = lock(&self.0.state);
            if state.generation != generation {
                return None;
            }
            state.failed()
        };
        let requests = self.clone();
        let clock = Arc::clone(clock);
        Some(thread::spawn(move || {
            clock.sleep(wait);
            let mut state = lock(&requests.0.state);
            if state.generation == generation {
                state.pending = true;
                drop(state);
                requests.0.wake.notify_one();
            }
        }))
    }
}

// A panic while holding the lock leaves whole fields, so the poison carries nothing.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `serve`'s scores thread: a rebuild for each request taken, saying what it did on `log`.
/// A rebuild that fails is logged and the list in use stays as it was. The thread asks
/// itself again an hour later on `clock`, as the dumps' loop does, and again after twice as
/// long for each failure in a row, up to a day; meanwhile it waits for requests as before,
/// and a new one starts the waits over.
pub fn serve(
    config: &Config,
    clock: &Arc<dyn Clock>,
    requests: &Requests,
    log: &mut dyn FnMut(&str),
) -> ! {
    serve_with(requests, clock, || {
        let result = run(config, &**clock, log);
        reported(&result, log)
    })
}

/// Says on `log` what a rebuild did, and whether it counts as done: a failure doesn't, except
/// the list built with only its directory sync failed, which is logged as an error all the
/// same but needs no retry.
fn reported(result: &Result<Summary, RescoreError>, log: &mut dyn FnMut(&str)) -> bool {
    match result {
        Ok(summary) => {
            log(&summary.to_string());
            true
        }
        Err(error) => {
            log(&format!("error: scores: {error}"));
            error.is_built()
        }
    }
}

/// Runs `rebuild` for each request taken, one at a time; one that returns false is
/// requested again after the retry wait, which grows with the failures in a row.
fn serve_with(requests: &Requests, clock: &Arc<dyn Clock>, mut rebuild: impl FnMut() -> bool) -> ! {
    loop {
        let generation = requests.take();
        if rebuild() {
            lock(&requests.0.state).failures = 0;
        } else {
            let _ = requests.retry_later(clock, generation);
        }
    }
}

/// `scored` as the scores database stores it, with what its row shows.
fn row(
    scored: &Scored,
    entry: &Entry,
    representative: Option<u64>,
    label: Option<Named>,
    styles: Vec<Named>,
) -> Row {
    let (kind, id) = scored.candidate.kind_id();
    Row {
        kind,
        id,
        representative,
        score: scored.score,
        title: entry.title.clone(),
        year: entry.year,
        artists: entry
            .artists
            .iter()
            .map(|(id, name)| Named::new(*id, name.clone()))
            .collect(),
        label,
        styles,
        reasons: scored.reasons.iter().map(stored_reason).collect(),
    }
}

fn stored_reason(reason: &Reason) -> stored::Reason {
    let of_kind = || stored::Reason {
        weight: reason.weight,
        kind: reason.connection.kind(),
        named: None,
        role: None,
        other: None,
        relation: None,
        albums: None,
        items: Vec::new(),
    };
    let albums = |albums: usize| Some(albums as u64);
    match &reason.connection {
        Connection::Artist { artist, albums: n } => stored::Reason {
            named: Some(artist.clone()),
            albums: albums(*n),
            ..of_kind()
        },
        Connection::Credit {
            artist,
            role,
            seed_roles,
            albums: n,
        } => stored::Reason {
            named: Some(artist.clone()),
            role: role.clone(),
            albums: albums(*n),
            items: seed_roles
                .iter()
                .map(|role| Item {
                    id: None,
                    name: Some(role.clone()),
                })
                .collect(),
            ..of_kind()
        },
        Connection::Label {
            label,
            via,
            albums: n,
        } => stored::Reason {
            named: Some(label.clone()),
            other: via.clone(),
            albums: albums(*n),
            ..of_kind()
        },
        Connection::Related {
            artist,
            relation,
            of,
        } => stored::Reason {
            named: Some(artist.clone()),
            other: Some(of.clone()),
            relation: Some(*relation),
            ..of_kind()
        },
        Connection::Styles { styles } => stored::Reason {
            items: styles
                .iter()
                .map(|style| Item {
                    id: Some(style.id),
                    name: style.name.clone(),
                })
                .collect(),
            ..of_kind()
        },
    }
}

/// A row of the scores database at `rank` as the score made it.
///
/// # Errors
///
/// When a reason lacks what its kind needs.
pub fn listed(rank: u64, row: Row) -> Result<Listed, RescoreError> {
    let candidate = Candidate::new(row.kind, row.id);
    let reasons = row
        .reasons
        .into_iter()
        .map(|reason| {
            Ok(Reason {
                weight: reason.weight,
                connection: connection(reason)?,
            })
        })
        .collect::<Result<_, RescoreError>>()?;
    Ok(Listed {
        rank,
        scored: Scored {
            candidate,
            score: row.score,
            reasons,
        },
        representative: row.representative,
        title: row.title,
        year: row.year,
        artists: row.artists,
        label: row.label,
        styles: row.styles,
    })
}

/// `value`, or the error that the scores hold `problem`.
fn need<T>(value: Option<T>, problem: &'static str) -> Result<T, RescoreError> {
    value.ok_or(RescoreError::Reason(problem))
}

/// A stored reason's connection, failing when it lacks what its kind needs.
fn connection(reason: stored::Reason) -> Result<Connection, RescoreError> {
    const NO_ARTIST: &str = "a reason without its artist or label";
    let (named, other) = (reason.named, reason.other);
    let albums = || {
        need(reason.albums, "a reason without its count of seeds")
            .and_then(|n| usize::try_from(n).map_err(|_| RescoreError::Reason("too many seeds")))
    };
    Ok(match reason.kind {
        ReasonKind::Artist => Connection::Artist {
            artist: need(named, NO_ARTIST)?,
            albums: albums()?,
        },
        ReasonKind::Credit => Connection::Credit {
            artist: need(named, NO_ARTIST)?,
            role: reason.role,
            seed_roles: reason
                .items
                .into_iter()
                .map(|item| need(item.name, "a credit's role without its name"))
                .collect::<Result<_, _>>()?,
            albums: albums()?,
        },
        ReasonKind::Label => Connection::Label {
            label: need(named, NO_ARTIST)?,
            via: other,
            albums: albums()?,
        },
        ReasonKind::Related => Connection::Related {
            artist: need(named, NO_ARTIST)?,
            relation: need(reason.relation, "a related artist without the relation")?,
            of: need(other, "a related artist without the seed's artist")?,
        },
        ReasonKind::Styles => Connection::Styles {
            styles: reason
                .items
                .into_iter()
                .map(|item| {
                    Ok(stored::Named {
                        id: need(item.id, "a style without its id")?,
                        name: item.name,
                    })
                })
                .collect::<Result<_, RescoreError>>()?,
        },
    })
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, SystemTime};

    use catalog::swap::SwapError;

    use super::{Requests, RescoreError, lock, reported, serve_with};
    use crate::clock::Clock;
    use crate::fetch::RETRY_AFTER_FAILURE;

    #[test]
    fn a_list_built_with_only_its_directory_sync_failed_is_logged_and_not_retried() {
        let mut log = Vec::new();
        let unsynced = Err(RescoreError::Swap(SwapError::Unsynced(
            "scores.sqlite".into(),
            std::io::Error::other("disk gone"),
        )));
        assert!(reported(&unsynced, &mut |line| log.push(line.to_string())));
        assert_eq!(log.len(), 1);
        assert!(log[0].starts_with("error: scores: "), "{log:?}");
        assert!(log[0].contains("disk gone"), "{log:?}");
        // Any other failure is retried.
        let failed = Err(RescoreError::Unswapped);
        assert!(!reported(&failed, &mut |_| {}));
    }

    #[test]
    fn requests_during_a_rebuild_make_one_more_run_not_one_each() {
        let requests = Requests::default();
        let clock: Arc<dyn Clock> = Arc::new(Gated::new().0);
        let (started, runs) = mpsc::channel();
        let (finish, gate) = mpsc::channel::<()>();
        let serving = requests.clone();
        thread::spawn(move || {
            let probe = serving.clone();
            serve_with(&serving, &clock, move || {
                // Whether a request is still waiting as this run starts.
                started.send(probe.pending()).unwrap();
                gate.recv().unwrap();
                true
            })
        });
        requests.request();
        assert_eq!(runs.recv(), Ok(false));
        // Two requests while the first run is under way.
        requests.request();
        requests.request();
        assert!(requests.pending());
        finish.send(()).unwrap();
        // The second run took both, and none is left for a third.
        assert_eq!(runs.recv(), Ok(false));
        assert!(!requests.pending());
        finish.send(()).unwrap();
    }

    /// A clock whose sleeps report their length and then wait to be let go, and whose time
    /// never moves.
    struct Gated {
        slept: Mutex<Sender<Duration>>,
        release: Mutex<Receiver<()>>,
    }

    impl Gated {
        /// The clock, the lengths of its sleeps, and the sender that lets one sleep go.
        fn new() -> (Self, Receiver<Duration>, Sender<()>) {
            let (slept, sleeps) = mpsc::channel();
            let (release, released) = mpsc::channel();
            let clock = Self {
                slept: Mutex::new(slept),
                release: Mutex::new(released),
            };
            (clock, sleeps, release)
        }
    }

    impl Clock for Gated {
        fn now(&self) -> SystemTime {
            SystemTime::UNIX_EPOCH
        }

        fn sleep(&self, duration: Duration) {
            self.slept.lock().unwrap().send(duration).unwrap();
            // A test that never lets it go leaves this thread waiting, which ends with the
            // test process.
            let _ = self.release.lock().unwrap().recv();
        }
    }

    /// A clock whose sleeps each report their length with a sender that lets that one go,
    /// and whose time never moves.
    struct Timers(Mutex<Sender<(Duration, Sender<()>)>>);

    impl Clock for Timers {
        fn now(&self) -> SystemTime {
            SystemTime::UNIX_EPOCH
        }

        fn sleep(&self, duration: Duration) {
            let (release, released) = mpsc::channel();
            self.0.lock().unwrap().send((duration, release)).unwrap();
            let _ = released.recv();
        }
    }

    #[test]
    fn a_new_request_supersedes_a_retry_that_is_waiting() {
        let (sleeping, sleeps) = mpsc::channel();
        let clock: Arc<dyn Clock> = Arc::new(Timers(Mutex::new(sleeping)));
        let requests = Requests::default();
        requests.request();
        let generation = requests.take();
        // Three failures in a row, so the fourth waits 8 hours.
        for _ in 0..3 {
            lock(&requests.0.state).failed();
        }
        let old = requests
            .retry_later(&clock, generation)
            .expect("a retry is waiting");
        let (wait, release_old) = sleeps.recv_timeout(WAIT).unwrap();
        assert_eq!(wait, Duration::from_hours(8));
        // A request starts the waits over, and its failure waits a retry of its own.
        requests.request();
        let generation = requests.take();
        let new = requests
            .retry_later(&clock, generation)
            .expect("a new request's failure waits its own retry");
        let (wait, release_new) = sleeps.recv_timeout(WAIT).unwrap();
        assert_eq!(wait, RETRY_AFTER_FAILURE);
        // The old sleeper wakes first and asks for nothing.
        release_old.send(()).unwrap();
        old.join().unwrap();
        assert!(!requests.pending());
        // The new one asks when its hour is up.
        release_new.send(()).unwrap();
        new.join().unwrap();
        assert!(requests.pending());
    }

    #[test]
    fn a_rebuild_that_fails_after_a_newer_request_waits_for_no_retry() {
        let (sleeping, sleeps) = mpsc::channel();
        let clock: Arc<dyn Clock> = Arc::new(Timers(Mutex::new(sleeping)));
        let requests = Requests::default();
        requests.request();
        let old = requests.take();
        // A request while that rebuild runs; it fails, and the request runs next.
        requests.request();
        assert!(requests.retry_later(&clock, old).is_none());
        assert!(requests.pending());
        // That one's failure is the first of its waits.
        let new = requests.take();
        let _retry = requests
            .retry_later(&clock, new)
            .expect("a retry is waiting");
        let (wait, _release) = sleeps.recv_timeout(WAIT).unwrap();
        assert_eq!(wait, RETRY_AFTER_FAILURE);
    }

    /// A scores thread whose rebuilds fail the first `failures` times; it says on the
    /// returned receiver each time a rebuild starts.
    fn failing(failures: usize, clock: Gated) -> (Requests, Receiver<usize>) {
        let requests = Requests::default();
        let clock: Arc<dyn Clock> = Arc::new(clock);
        let (started, runs) = mpsc::channel();
        let serving = requests.clone();
        thread::spawn(move || {
            let mut run = 0;
            serve_with(&serving, &clock, move || {
                run += 1;
                started.send(run).unwrap();
                run > failures
            })
        });
        (requests, runs)
    }

    /// How long a test waits for what it expects, so that a missing one fails it.
    const WAIT: Duration = Duration::from_secs(10);

    #[test]
    fn a_failed_rebuild_is_tried_again_after_the_retry_wait() {
        let (clock, sleeps, release) = Gated::new();
        let (requests, runs) = failing(1, clock);
        requests.request();
        assert_eq!(runs.recv_timeout(WAIT).map_err(|_| "no run"), Ok(1));
        // Nothing asks again; the thread waits on the clock for this long, and then runs.
        assert_eq!(
            sleeps.recv_timeout(WAIT).map_err(|_| "no sleep"),
            Ok(RETRY_AFTER_FAILURE)
        );
        assert!(!requests.pending());
        release.send(()).unwrap();
        assert_eq!(runs.recv_timeout(WAIT).map_err(|_| "no run"), Ok(2));
        // The second run went well, so nothing is waiting to run a third.
        assert!(sleeps.try_recv().is_err());
    }

    /// A scores thread whose rebuilds go as `outcomes` say, true for one that works; it says
    /// on the returned receiver each time a rebuild starts.
    fn scripted(outcomes: Vec<bool>, clock: Gated) -> (Requests, Receiver<usize>) {
        let requests = Requests::default();
        let clock: Arc<dyn Clock> = Arc::new(clock);
        let (started, runs) = mpsc::channel();
        let serving = requests.clone();
        thread::spawn(move || {
            let mut run = 0;
            serve_with(&serving, &clock, move || {
                run += 1;
                started.send(run).unwrap();
                outcomes[run - 1]
            })
        });
        (requests, runs)
    }

    #[test]
    fn rebuilds_that_keep_failing_wait_twice_as_long_each_time_up_to_a_day() {
        let (clock, sleeps, release) = Gated::new();
        let (requests, runs) = scripted(vec![false; 8], clock);
        requests.request();
        let mut waits = Vec::new();
        for run in 1..=7 {
            assert_eq!(runs.recv_timeout(WAIT).map_err(|_| "no run"), Ok(run));
            waits.push(sleeps.recv_timeout(WAIT).map_err(|_| "no sleep").unwrap());
            release.send(()).unwrap();
        }
        let hours = |hours| Duration::from_hours(hours);
        assert_eq!(
            waits,
            [
                hours(1),
                hours(2),
                hours(4),
                hours(8),
                hours(16),
                hours(24),
                hours(24)
            ]
        );
    }

    #[test]
    fn a_rebuild_that_works_starts_the_waits_over() {
        let (clock, sleeps, release) = Gated::new();
        let (requests, runs) = scripted(vec![false, false, true, false], clock);
        requests.request();
        for (run, wait) in [(1, 1), (2, 2)] {
            assert_eq!(runs.recv_timeout(WAIT).map_err(|_| "no run"), Ok(run));
            assert_eq!(
                sleeps.recv_timeout(WAIT).map_err(|_| "no sleep"),
                Ok(Duration::from_hours(wait))
            );
            release.send(()).unwrap();
        }
        // The third run works; a later request that fails waits as after a first failure.
        assert_eq!(runs.recv_timeout(WAIT).map_err(|_| "no run"), Ok(3));
        requests.request();
        assert_eq!(runs.recv_timeout(WAIT).map_err(|_| "no run"), Ok(4));
        assert_eq!(
            sleeps.recv_timeout(WAIT).map_err(|_| "no sleep"),
            Ok(RETRY_AFTER_FAILURE)
        );
    }

    #[test]
    fn a_new_request_starts_the_waits_over() {
        let requests = Requests::default();
        let failed = || lock(&requests.0.state).failed();
        assert_eq!(failed(), Duration::from_hours(1));
        assert_eq!(failed(), Duration::from_hours(2));
        assert_eq!(failed(), Duration::from_hours(4));
        // An import or a profile asks again.
        requests.request();
        assert_eq!(failed(), Duration::from_hours(1));
        // The long run of failures, past what a shift can hold, stays at a day.
        for _ in 0..100 {
            failed();
        }
        assert_eq!(failed(), Duration::from_hours(24));
    }

    #[test]
    fn a_request_during_the_retry_wait_runs_at_once() {
        let (clock, sleeps, release) = Gated::new();
        let (requests, runs) = failing(1, clock);
        requests.request();
        assert_eq!(runs.recv_timeout(WAIT).map_err(|_| "no run"), Ok(1));
        assert_eq!(
            sleeps.recv_timeout(WAIT).map_err(|_| "no sleep"),
            Ok(RETRY_AFTER_FAILURE)
        );
        // The retry is still waiting on the clock.
        requests.request();
        assert_eq!(runs.recv_timeout(WAIT).map_err(|_| "no run"), Ok(2));
        release.send(()).unwrap();
    }
}
