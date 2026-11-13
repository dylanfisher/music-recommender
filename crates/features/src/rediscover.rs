//! Rediscover (docs/api.md, Rediscover; ADR 0034): the kept profile's albums scored by how
//! long since they were played, by the community values against the profile's own, and by
//! the owner's loves, ratings and skips; then the day's albums drawn from the best scored,
//! the same all day, with at most 2 of an artist and of a label. Scored at each request from
//! the kept profile, the catalog and the community database; only the seeds' read of the
//! catalog is kept in memory, until the catalog or the profile's tagged releases change.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use catalog::community::{Reading, Stored, Values};
use catalog::discogs::{Catalog, CatalogError};
use catalog::score::digest;
use config::Config;
use primitives::fold;

use crate::clock::{Clock, unix_seconds};
use crate::community::{self, ReadError};
use crate::community_factor::{count, measured_factors};
use crate::library::artist_key;
use crate::profile::{self, Album, Profile, ProfileError};
use crate::rescore::label;
use crate::score::Named;
use crate::seed::Seeds;

mod relation;

use relation::Relations;
pub use relation::{Link, Thread};

// The constants below are first values, which the owner's checkpoint judges (ADR 0034).
/// The most albums a day.
pub(crate) const DRAWN: usize = 60;
/// The day's albums are drawn from this many of the best scored.
const POOL: usize = 600;
/// The most albums of one artist, and of one label, a day.
const CAP: usize = 2;
/// An album played within this many days is never among the day's albums; the relations
/// link to the albums played within them.
pub(crate) const RECENT_DAYS: i64 = 30;
/// The days over which an album played comes back: after this many it scores `1 − 1/e`,
/// about 0.63, of an album never played.
const COMES_BACK_DAYS: f64 = 180.0;
/// What loved tracks, or a rating of 4 or 5, multiply the score by.
const LOVED: f64 = 1.3;
/// What an album whose every play was a skip loses of its score; one with as many skips as
/// plays loses half of it.
const SKIPPED: f64 = 0.5;
/// A community factor at least this is a reason of its own.
const WELL_RATED: f64 = 1.2;
/// The most reasons an album shows.
const REASONS: usize = 3;
/// Seconds in a day.
const DAY: i64 = 86_400;

/// The player's local date, `YYYY-MM-DD`, a real day of the Gregorian calendar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Day(String);

impl Day {
    /// `text` as a day, or `None` when it isn't `YYYY-MM-DD` or not a real date.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        let digits = |range: std::ops::Range<usize>| {
            bytes[range.clone()]
                .iter()
                .all(u8::is_ascii_digit)
                .then(|| text[range].parse::<u32>().ok())
                .flatten()
        };
        if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
            return None;
        }
        let (year, month, day) = (digits(0..4)?, digits(5..7)?, digits(8..10)?);
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let days = match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if leap => 29,
            2 => 28,
            _ => return None,
        };
        (1..=days).contains(&day).then(|| Self(text.to_string()))
    }

    /// The day as it was sent.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The service's name for an album, the same in every profile and from every Mac: its
/// release id, or for an untagged album a hash of its folded artist and title.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Id {
    /// `r` and the Discogs release id.
    Release(u64),
    /// `k` and 16 lowercase hex digits.
    Key(u64),
}

impl Id {
    /// The id of `album`.
    #[must_use]
    pub fn of(album: &Album) -> Self {
        if let Some(release_id) = album.discogs_release_id {
            return Self::Release(release_id);
        }
        let names = format!(
            "{}\n{}",
            fold::key(&album.artist, false),
            fold::key(&album.title, true)
        );
        Self::Key(hash(&names))
    }

    /// `text` as an id, or `None` when it is neither shape: `r` and a release id from 1 to
    /// 9223372036854775807, or `k` and 16 lowercase hex digits.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        if let Some(digits) = text.strip_prefix('r') {
            let canonical = digits.bytes().all(|b| b.is_ascii_digit()) && !digits.starts_with('0');
            let id = digits.parse::<u64>().ok()?;
            (canonical && id <= i64::MAX.cast_unsigned()).then_some(Self::Release(id))
        } else if let Some(hex) = text.strip_prefix('k') {
            let canonical = hex.len() == 16
                && hex
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
            canonical
                .then(|| u64::from_str_radix(hex, 16).ok())
                .flatten()
                .map(Self::Key)
        } else {
            None
        }
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Release(id) => write!(f, "r{id}"),
            Self::Key(hash) => write!(f, "k{hash:016x}"),
        }
    }
}

/// What a day's Rediscover holds.
#[derive(Debug)]
pub struct Rediscover {
    /// The day asked.
    pub day: Day,
    /// When the scores were computed, in Unix seconds: at the request.
    pub computed_at: i64,
    /// The profile's albums, before the draw and the not-now marks.
    pub albums_considered: usize,
    /// The day's albums, best first.
    pub albums: Vec<Surfaced>,
    /// What two or more of them share, strongest first.
    pub threads: Vec<Thread>,
}

/// One of the day's albums.
#[derive(Debug)]
pub struct Surfaced {
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
    /// From 0 to 1, 1 for the profile's best scored album.
    pub score: f64,
    /// What the community database holds for its release, `None` when untagged.
    pub community: Option<Stored>,
    /// Why it is here, strongest first, at most `REASONS`.
    pub reasons: Vec<Reason>,
}

/// Why an album is among the day's.
#[derive(Debug, Clone, PartialEq)]
pub enum Reason {
    /// No track of it was ever played.
    NeverPlayed,
    /// It was last played this many whole days ago.
    NotPlayedSince(i64),
    /// Tracks of it are loved, or it is rated 4 or 5.
    Loved {
        /// Its loved tracks.
        loved: u32,
        /// The owner's rating.
        rating: Option<u8>,
    },
    /// The community rates it well or owns it widely.
    Community(Values),
    /// It shares something with albums played in the last `RECENT_DAYS` days.
    Link(Link),
}

impl Reason {
    /// The kind's name in the contract.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::NeverPlayed => "never_played",
            Self::NotPlayedSince(_) => "not_played_since",
            Self::Loved { .. } => "loved",
            Self::Community(_) => "community",
            Self::Link(link) => link.kind.name(),
        }
    }

    /// The reason in words, for showing.
    #[must_use]
    pub fn words(&self) -> String {
        match self {
            Self::NeverPlayed => "Never played".to_string(),
            Self::NotPlayedSince(days) => {
                let (n, unit) = if *days >= 365 {
                    (days / 365, "year")
                } else {
                    (days / 30, "month")
                };
                format!("Last played {n} {unit}{} ago", plural(n))
            }
            Self::Loved { loved, rating } => {
                let loved = (*loved > 0)
                    .then(|| format!("{loved} loved track{}", plural(i64::from(*loved))));
                let rated = rating
                    .filter(|rating| *rating >= 4)
                    .map(|rating| format!("rated {rating} by you"));
                let words = [rated, loved].into_iter().flatten().collect::<Vec<_>>();
                capitalized(&words.join(", "))
            }
            Self::Community(values) => {
                let rated = (values.rating_count > 0).then(|| {
                    format!(
                        "rated {:.1} by {}",
                        values.rating_average, values.rating_count
                    )
                });
                let owned = format!("{} own it", values.have);
                let words = [rated, Some(owned)]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>();
                capitalized(&format!("{} on Discogs", words.join(", ")))
            }
            Self::Link(link) => link.words(),
        }
    }
}

fn plural(n: i64) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn capitalized(words: &str) -> String {
    let mut chars = words.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// Why Rediscover couldn't be computed.
#[derive(Debug)]
pub enum RediscoverError {
    /// No profile has been kept.
    NoProfile,
    /// The kept profile can't be read.
    Profile(ProfileError),
    /// The catalog failed.
    Catalog(CatalogError),
    /// The community values can't be read.
    Community(ReadError),
}

impl fmt::Display for RediscoverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoProfile => f.write_str("no profile yet; the player sends one"),
            Self::Profile(error) => error.fmt(f),
            Self::Catalog(error) => error.fmt(f),
            Self::Community(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for RediscoverError {}

impl From<CatalogError> for RediscoverError {
    fn from(error: CatalogError) -> Self {
        Self::Catalog(error)
    }
}

/// The seeds' read of the catalog for the last profile Rediscover scored, which a service
/// shares among its request threads: most of a reply's time without it, and the same until
/// the catalog or the releases the profile is tagged with change. Clones share it.
#[derive(Debug, Clone, Default)]
pub struct Kept(Arc<Mutex<Option<KeptSeeds>>>);

/// What `Kept` holds: the seeds and what they were read for.
#[derive(Debug)]
struct KeptSeeds {
    /// The catalog's set and swap time and a digest of the profile's release ids.
    read_for: String,
    seeds: Arc<Seeds>,
}

impl Kept {
    /// The seeds of `profile`'s albums in `catalog`, read again only when the catalog's set
    /// or swap time or the profile's release ids differ from the last read. A second request
    /// meanwhile waits for the read rather than starting its own.
    fn seeds(&self, profile: &Profile, catalog: &Catalog) -> Result<Arc<Seeds>, CatalogError> {
        let summary = catalog.summary()?;
        let mut release_ids: Vec<u64> = profile
            .albums
            .iter()
            .filter_map(|album| album.discogs_release_id)
            .collect();
        release_ids.sort_unstable();
        release_ids.dedup();
        let ids: Vec<String> = release_ids.iter().map(u64::to_string).collect();
        let key = format!(
            "{:?} {:?} {}",
            summary.imported.first().map(|set| &set.date),
            summary.swapped_at,
            digest(&ids.join(" "))
        );
        let mut kept = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(found) = kept.as_ref()
            && found.read_for == key
        {
            return Ok(Arc::clone(&found.seeds));
        }
        let seeds = Arc::new(Seeds::read(profile, catalog)?);
        *kept = Some(KeptSeeds {
            read_for: key,
            seeds: Arc::clone(&seeds),
        });
        Ok(seeds)
    }
}

/// The day's albums of the profile kept in `config`'s data directory, scored at the
/// clock's time, with the seeds' read from `kept`.
///
/// # Errors
///
/// When no profile is kept, or the profile, the catalog or the community database can't be
/// read. No catalog, or one of another shape, gives albums without links, and no community
/// database albums without values.
pub fn rediscover(
    config: &Config,
    clock: &dyn Clock,
    kept: &Kept,
    day: &Day,
) -> Result<Rediscover, RediscoverError> {
    let now = unix_seconds(clock.now());
    let profile = profile::kept(&config.data_dir)
        .map_err(RediscoverError::Profile)?
        .ok_or(RediscoverError::NoProfile)?;
    let catalog = match Catalog::open_read_only(&config.data_dir)? {
        Reading::Open(catalog) => Some(catalog),
        Reading::Missing | Reading::OtherShape => None,
    };
    let seeds = match &catalog {
        Some(catalog) => kept.seeds(&profile, catalog)?,
        None => Arc::new(Seeds::default()),
    };
    let release_ids: Vec<u64> = profile
        .albums
        .iter()
        .filter_map(|album| album.discogs_release_id)
        .collect();
    let community: HashMap<u64, Stored> = release_ids
        .iter()
        .copied()
        .zip(community::stored(config, &release_ids).map_err(RediscoverError::Community)?)
        .collect();
    let measured = measure(&profile.albums, &community, &seeds);
    let scored = scores(&profile.albums, &measured, config.community_weight, now);
    let pool = pool(&profile.albums, &scored, &HashSet::new());
    let mut labels = HashMap::new();
    let mut pool_labels = Vec::with_capacity(pool.len());
    for &(index, _) in &pool {
        let release_id = profile.albums[index].discogs_release_id;
        let first = first_label(catalog.as_ref(), &mut labels, release_id)?;
        pool_labels.push(first.map(|label| label.id));
    }
    let drawn = pick(&profile.albums, &scored, &pool, &pool_labels, day);
    let relations = Relations::new(&seeds, &profile.albums, now);
    let mut albums = Vec::with_capacity(drawn.len());
    for index in drawn {
        let album = &profile.albums[index];
        let release_id = album.discogs_release_id;
        let links = match (&catalog, release_id) {
            (Some(catalog), Some(release_id)) => catalog.links(release_id)?,
            _ => None,
        };
        let label = first_label(catalog.as_ref(), &mut labels, release_id)?;
        let styles = links.as_ref().map_or_else(Vec::new, |links| {
            links
                .styles
                .iter()
                .map(|&style| Named::new(u64::from(style), seeds.style_names.get(&style).cloned()))
                .collect()
        });
        let Scored { score, factor, .. } = scored[index];
        albums.push(Surfaced {
            id: Id::of(album),
            album: album.clone(),
            year: links.as_ref().and_then(|links| links.year),
            label,
            styles,
            score,
            community: release_id.and_then(|id| community.get(&id).copied()),
            reasons: reasons(
                album,
                measured[index].values,
                factor,
                relations.links(release_id),
                now,
            ),
        });
    }
    let day_albums: Vec<(Id, Option<u64>)> = albums
        .iter()
        .map(|surfaced| (surfaced.id, surfaced.album.discogs_release_id))
        .collect();
    Ok(Rediscover {
        day: day.clone(),
        computed_at: now,
        albums_considered: profile.albums.len(),
        albums,
        threads: relations.threads(&day_albums),
    })
}

/// What the community factor measures each of `albums` by: its release's values in
/// `community` and its styles among `seeds`.
fn measure<'a>(
    albums: &[Album],
    community: &HashMap<u64, Stored>,
    seeds: &'a Seeds,
) -> Vec<Measured<'a>> {
    let styles: HashMap<u64, &[u32]> = seeds
        .seeds
        .iter()
        .map(|seed| (seed.release_id, seed.styles.as_slice()))
        .collect();
    albums
        .iter()
        .map(|album| {
            let release_id = album.discogs_release_id;
            Measured {
                values: release_id
                    .and_then(|id| community.get(&id))
                    .and_then(values_of),
                styles: release_id
                    .and_then(|id| styles.get(&id).copied())
                    .unwrap_or_default(),
            }
        })
        .collect()
}

/// The first label of the release `release_id` in `catalog`, as an album of a list shows
/// it ([`label`]); `None` without a catalog, a release or a label.
fn first_label(
    catalog: Option<&Catalog>,
    labels: &mut HashMap<u64, Option<(String, Option<u64>)>>,
    release_id: Option<u64>,
) -> Result<Option<Named>, CatalogError> {
    let (Some(catalog), Some(release_id)) = (catalog, release_id) else {
        return Ok(None);
    };
    match catalog.first_label(release_id)? {
        Some(first) => label(catalog, labels, first),
        None => Ok(None),
    }
}

/// The values a stored row has for the score: a fetched release's, and a failed one's
/// from an earlier fetch, as the recommendations read them (ADR 0032).
fn values_of(stored: &Stored) -> Option<Values> {
    match *stored {
        Stored::Fetched { values, .. }
        | Stored::Failed {
            values: Some(values),
            ..
        } => Some(values),
        _ => None,
    }
}

/// What the community factor measures an album by.
#[derive(Debug, Clone, Default)]
pub(crate) struct Measured<'a> {
    /// Its release's community values, `None` when untagged or not fetched.
    pub(crate) values: Option<Values>,
    /// Its release's styles, for the rating's prior.
    pub(crate) styles: &'a [u32],
}

/// An album's score and its parts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Scored {
    /// From 0 to 1, 1 for the best; 0 for an album played within `RECENT_DAYS`, which
    /// takes no part.
    pub(crate) score: f64,
    /// Its community factor.
    pub(crate) factor: f64,
}

/// Each of `albums`' scores at `now`, in their order: how long since it was played, times
/// its community factor at `weight` measured against the albums with values, times what
/// its loves, rating and skips make of it; then scaled so the best is 1.
pub(crate) fn scores(
    albums: &[Album],
    measured: &[Measured<'_>],
    weight: f64,
    now: i64,
) -> Vec<Scored> {
    let items: Vec<(Option<&Values>, &[u32])> = measured
        .iter()
        .map(|measured| (measured.values.as_ref(), measured.styles))
        .collect();
    let factors = measured_factors(&items, weight).unwrap_or_else(|| vec![1.0; albums.len()]);
    let mut scored: Vec<Scored> = albums
        .iter()
        .zip(factors)
        .map(|(album, factor)| Scored {
            score: staleness(album.last_played, now) * factor * owner(album),
            factor,
        })
        .collect();
    let best = scored.iter().map(|scored| scored.score).fold(0.0, f64::max);
    if best > 0.0 {
        for scored in &mut scored {
            scored.score /= best;
        }
    }
    scored
}

/// 1 for an album never played; else `1 − exp(−days / COMES_BACK_DAYS)` for one last played
/// `days` before `now`, and 0 within `RECENT_DAYS`, a time after `now` included.
fn staleness(last_played: Option<i64>, now: i64) -> f64 {
    let Some(last_played) = last_played else {
        return 1.0;
    };
    let seconds = now.saturating_sub(last_played);
    if seconds < RECENT_DAYS * DAY {
        return 0.0;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "seconds since a play far below 2^52"
    )]
    let days = seconds as f64 / DAY as f64;
    1.0 - (-days / COMES_BACK_DAYS).exp()
}

/// What the owner's loves, rating and skips make of an album's score: `LOVED` for loved
/// tracks or a rating of 4 or 5, and less by `SKIPPED` times its skips' share of its plays
/// and skips.
fn owner(album: &Album) -> f64 {
    let loved = if is_loved(album) { LOVED } else { 1.0 };
    let listened = album.plays.saturating_add(album.skips);
    let skipped = if listened == 0 {
        0.0
    } else {
        count(album.skips) / count(listened)
    };
    loved * (1.0 - SKIPPED * skipped)
}

fn is_loved(album: &Album) -> bool {
    album.loved > 0 || album.rating.is_some_and(|rating| rating >= 4)
}

/// The albums of `albums` the day's are drawn from, with their ids, best first: of each id,
/// the best scored album alone; of those, the best `POOL` that score above 0 and aren't
/// `excluded`.
pub(crate) fn pool(
    albums: &[Album],
    scored: &[Scored],
    excluded: &HashSet<Id>,
) -> Vec<(usize, Id)> {
    let mut order: Vec<(usize, Id)> = albums
        .iter()
        .enumerate()
        .filter(|&(index, _)| scored[index].score > 0.0)
        .map(|(index, album)| (index, Id::of(album)))
        .collect();
    order.sort_by(|a, b| {
        scored[b.0]
            .score
            .total_cmp(&scored[a.0].score)
            .then(a.1.cmp(&b.1))
    });
    let mut seen = HashSet::new();
    order.retain(|(_, id)| seen.insert(*id) && !excluded.contains(id));
    order.truncate(POOL);
    order
}

/// The indexes of the day's albums of `pool`, best first: drawn in an order weighted by
/// score and set by a hash of `day` and the id, the first `DRAWN` that keep `CAP` an artist
/// and a label. `labels` holds each pool album's label, in the pool's order.
pub(crate) fn pick(
    albums: &[Album],
    scored: &[Scored],
    pool: &[(usize, Id)],
    labels: &[Option<u64>],
    day: &Day,
) -> Vec<usize> {
    // Weighted sampling without replacement (Efraimidis and Spirakis): each album's key is
    // `u^(1 / score)` for its own `u` from 0 to 1, and the order by key is a draw weighted
    // by score. Compared by its log, `ln u / score`, which doesn't underflow.
    let mut keyed: Vec<(f64, usize, Id, Option<u64>)> = pool
        .iter()
        .zip(labels)
        .map(|(&(index, id), &label)| {
            (
                uniform(day, id).ln() / scored[index].score,
                index,
                id,
                label,
            )
        })
        .collect();
    keyed.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.2.cmp(&b.2)));
    let mut artists: HashMap<String, usize> = HashMap::new();
    let mut on_label: HashMap<u64, usize> = HashMap::new();
    let mut drawn: Vec<(usize, Id)> = Vec::with_capacity(DRAWN);
    for (_, index, id, label) in keyed {
        if drawn.len() == DRAWN {
            break;
        }
        // Various Artists and an album with no artist count for no artist.
        let artist = artist_key(&albums[index].artist);
        let full = |n: Option<&usize>| n.is_some_and(|&n| n >= CAP);
        if artist
            .as_ref()
            .is_some_and(|artist| full(artists.get(artist)))
            || label.is_some_and(|label| full(on_label.get(&label)))
        {
            continue;
        }
        if let Some(artist) = artist {
            *artists.entry(artist).or_default() += 1;
        }
        if let Some(label) = label {
            *on_label.entry(label).or_default() += 1;
        }
        drawn.push((index, id));
    }
    drawn.sort_by(|a, b| {
        scored[b.0]
            .score
            .total_cmp(&scored[a.0].score)
            .then(a.1.cmp(&b.1))
    });
    drawn.into_iter().map(|(index, _)| index).collect()
}

/// A number from 0 to 1, both left out, from `day` and `id`: the same for the same two, and
/// unrelated for another day.
fn uniform(day: &Day, id: Id) -> f64 {
    // The top 52 bits, plus a half, over 2^52: exact in an f64, and never 0 or 1.
    #[expect(clippy::cast_precision_loss, reason = "52 bits fit an f64 exactly")]
    let top = (hash(&format!("{}\n{id}", day.as_str())) >> 12) as f64;
    (top + 0.5) / 4_503_599_627_370_496.0
}

/// The first 64 bits of `text`'s [`digest`], stable across builds.
fn hash(text: &str) -> u64 {
    // `digest` is 32 hex digits, so the first 16 are a `u64`.
    u64::from_str_radix(&digest(text)[..16], 16).unwrap_or_default()
}

/// `album`'s reasons, strongest first, at most `REASONS`: how long since it was played, its
/// strongest link to the albums played recently, its loves and rating, its community values
/// when their factor is at least `WELL_RATED`, and its other links.
fn reasons(
    album: &Album,
    values: Option<Values>,
    factor: f64,
    links: Vec<Link>,
    now: i64,
) -> Vec<Reason> {
    let mut links = links.into_iter().map(Reason::Link);
    let mut reasons = vec![match album.last_played {
        None => Reason::NeverPlayed,
        Some(last_played) => Reason::NotPlayedSince(now.saturating_sub(last_played) / DAY),
    }];
    reasons.extend(links.next());
    if is_loved(album) {
        reasons.push(Reason::Loved {
            loved: album.loved,
            rating: album.rating,
        });
    }
    if let Some(values) = values.filter(|_| factor >= WELL_RATED) {
        reasons.push(Reason::Community(values));
    }
    reasons.extend(links);
    reasons.truncate(REASONS);
    reasons
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use catalog::community::Values;

    use super::{
        CAP, DAY, DRAWN, Day, Id, Measured, POOL, RECENT_DAYS, Scored, pick, pool, scores,
    };
    use crate::profile::Album;

    /// The tests' time, in Unix seconds.
    const NOW: i64 = 1_800_000_000;

    fn album(artist: &str, title: &str, release_id: Option<u64>) -> Album {
        Album {
            key: format!("{artist}/{title}"),
            artist: artist.to_string(),
            title: title.to_string(),
            track_count: 1,
            durations: vec![60],
            catalog_numbers: Vec::new(),
            barcodes: Vec::new(),
            discogs_release_id: release_id,
            plays: 0,
            loved: 0,
            rating: None,
            last_played: None,
            skips: 0,
        }
    }

    fn values(rating_average: f64, rating_count: u64, have: u64, want: u64) -> Values {
        Values {
            rating_average,
            rating_count,
            have,
            want,
            num_for_sale: None,
            lowest_price: None,
        }
    }

    /// `albums`' scores at `NOW`, at weight 1, with `values` by position.
    fn scored(albums: &[Album], values: &[Option<Values>]) -> Vec<f64> {
        let measured: Vec<Measured<'_>> = (0..albums.len())
            .map(|index| Measured {
                values: values.get(index).copied().flatten(),
                styles: &[],
            })
            .collect();
        scores(albums, &measured, 1.0, NOW)
            .into_iter()
            .map(|scored| scored.score)
            .collect()
    }

    #[test]
    fn never_played_beats_played_long_ago_and_played_yesterday_takes_no_part() {
        let played = |title, days_ago: i64| Album {
            last_played: Some(NOW - days_ago * DAY),
            plays: 5,
            ..album("A", title, None)
        };
        let albums = [
            album("A", "Never", None),
            played("Three years", 3 * 365),
            played("Two months", 60),
            played("Yesterday", 1),
            played("Just inside", RECENT_DAYS - 1),
        ];
        let scores = scored(&albums, &[]);
        assert!((scores[0] - 1.0).abs() < 1e-12, "{scores:?}");
        assert!(scores[0] > scores[1] && scores[1] > scores[2], "{scores:?}");
        // 60 days of 180: 1 − e^(−1/3).
        assert!((scores[2] - (1.0 - (-1.0f64 / 3.0).exp())).abs() < 1e-9);
        assert_eq!(scores[3], 0.0);
        assert_eq!(scores[4], 0.0);
    }

    #[test]
    fn a_four_point_six_from_800_votes_beats_a_five_from_2() {
        let mut albums = vec![
            album("A", "Two votes", Some(1)),
            album("B", "Many", Some(2)),
        ];
        let mut known = vec![
            Some(values(5.0, 2, 300, 100)),
            Some(values(4.6, 800, 300, 100)),
        ];
        for id in 3..30 {
            albums.push(album("C", &format!("Typical {id}"), Some(id)));
            known.push(Some(values(3.8, 100, 300, 100)));
        }
        let scores = scored(&albums, &known);
        assert!(scores[1] > scores[0], "{scores:?}");
        assert!((scores[1] - 1.0).abs() < 1e-12, "{scores:?}");
    }

    #[test]
    fn an_album_without_values_counts_as_typical() {
        let albums = [
            album("A", "Typical", Some(1)),
            album("B", "Untagged", None),
            album("C", "Unfetched", Some(3)),
        ];
        // One album with values is the typical one, a factor of 1, as are the two without.
        let scores = scored(&albums, &[Some(values(4.0, 50, 100, 20))]);
        assert_eq!(scores, [1.0, 1.0, 1.0]);
    }

    #[test]
    fn loves_and_a_high_rating_raise_an_album_and_skips_lower_it() {
        let albums = [
            album("A", "Plain", None),
            Album {
                loved: 2,
                ..album("A", "Loved", None)
            },
            Album {
                rating: Some(4),
                ..album("A", "Rated 4", None)
            },
            Album {
                rating: Some(3),
                ..album("A", "Rated 3", None)
            },
            Album {
                plays: 10,
                skips: 10,
                ..album("A", "Half skipped", None)
            },
        ];
        let scores = scored(&albums, &[]);
        assert!((scores[1] - 1.0).abs() < 1e-12, "{scores:?}");
        assert!((scores[2] - 1.0).abs() < 1e-12, "{scores:?}");
        assert!(scores[3] < scores[2] && (scores[3] - scores[0]).abs() < 1e-12);
        assert!((scores[4] / scores[0] - 0.75).abs() < 1e-12, "{scores:?}");
    }

    /// The day's albums of `albums`, each scored `score_of` its index, without labels.
    fn drawn(albums: &[Album], day: &str, score_of: impl Fn(usize) -> f64) -> Vec<String> {
        let scored: Vec<Scored> = (0..albums.len())
            .map(|index| Scored {
                score: score_of(index),
                factor: 1.0,
            })
            .collect();
        let pool = pool(albums, &scored, &HashSet::new());
        let labels = vec![None; pool.len()];
        let day = Day::parse(day).unwrap();
        pick(albums, &scored, &pool, &labels, &day)
            .into_iter()
            .map(|index| albums[index].title.clone())
            .collect()
    }

    /// `n` albums, each by an artist of its own.
    fn many(n: u64) -> Vec<Album> {
        (1..=n)
            .map(|id| album(&format!("Artist {id}"), &format!("Album {id}"), Some(id)))
            .collect()
    }

    #[test]
    fn the_same_day_draws_the_same_albums_and_another_day_others() {
        let albums = many(300);
        let score = |index| 1.0 - f64::from(u32::try_from(index).unwrap()) / 1000.0;
        let today = drawn(&albums, "2026-11-12", score);
        assert_eq!(today.len(), DRAWN);
        assert_eq!(drawn(&albums, "2026-11-12", score), today);
        let tomorrow = drawn(&albums, "2026-11-13", score);
        let shared = tomorrow
            .iter()
            .filter(|title| today.contains(title))
            .count();
        assert!(shared < DRAWN / 2, "{shared} of {DRAWN} the same");
    }

    #[test]
    fn the_draw_favors_the_best_scored_and_keeps_to_the_pool() {
        // The first 10 score 1, the rest a hundredth of it.
        let albums = many(2 * POOL as u64);
        let mut best = 0;
        for day in 1..=9 {
            let titles = drawn(&albums, &format!("2026-11-0{day}"), |index| {
                if index < 10 { 1.0 } else { 0.01 }
            });
            best += titles
                .iter()
                .filter(|title| title.trim_start_matches("Album ").parse::<u64>().unwrap() <= 10)
                .count();
            // Only the best `POOL` take part.
            assert!(titles.iter().all(|title| {
                title.trim_start_matches("Album ").parse::<usize>().unwrap() <= POOL
            }));
        }
        // Nearly every one of the 10 every day, against 1 in 10 of the rest.
        assert!(best > 80, "{best} of 90");
    }

    #[test]
    fn at_most_2_of_an_artist_and_of_a_label_a_day() {
        let mut albums: Vec<Album> = (1..=10)
            .map(|id| album("The Same Artist", &format!("Same artist {id}"), Some(id)))
            .collect();
        albums.extend(
            (11..=20).map(|id| album(&format!("A{id}"), &format!("Same label {id}"), Some(id))),
        );
        albums.extend(
            (21..=30).map(|id| album("Various Artists", &format!("Various {id}"), Some(id))),
        );
        let scored = vec![
            Scored {
                score: 1.0,
                factor: 1.0
            };
            albums.len()
        ];
        let pool = pool(&albums, &scored, &HashSet::new());
        let labels: Vec<Option<u64>> = pool
            .iter()
            .map(|&(index, _)| (10..20).contains(&index).then_some(7))
            .collect();
        let day = Day::parse("2026-11-12").unwrap();
        let titles: Vec<String> = pick(&albums, &scored, &pool, &labels, &day)
            .into_iter()
            .map(|index| albums[index].title.clone())
            .collect();
        let count = |prefix: &str| {
            titles
                .iter()
                .filter(|title| title.starts_with(prefix))
                .count()
        };
        assert_eq!(count("Same artist"), CAP, "{titles:?}");
        assert_eq!(count("Same label"), CAP, "{titles:?}");
        // "The" is folded away, and Various Artists is no artist to cap.
        assert_eq!(count("Various"), 10, "{titles:?}");
    }

    #[test]
    fn two_albums_with_one_id_are_drawn_once_and_a_marked_one_not_at_all() {
        let albums = [
            album("A", "One", Some(1)),
            album("B", "Two", Some(1)),
            album("C", "Untagged", None),
            album("C", "Untagged (Remastered)", None),
            album("D", "Marked", Some(4)),
        ];
        let scored = vec![
            Scored {
                score: 1.0,
                factor: 1.0
            };
            albums.len()
        ];
        let excluded = HashSet::from([Id::Release(4)]);
        let pool = pool(&albums, &scored, &excluded);
        let ids: Vec<String> = pool.iter().map(|(_, id)| id.to_string()).collect();
        assert_eq!(ids.len(), 2, "{ids:?}");
        assert!(ids.contains(&"r1".to_string()));
        assert!(
            ids.iter().any(|id| id.starts_with('k') && id.len() == 17),
            "{ids:?}"
        );
    }

    #[test]
    fn a_day_is_a_real_date_in_its_one_shape() {
        for day in ["2026-11-12", "2024-02-29", "2000-02-29", "1999-12-31"] {
            assert_eq!(Day::parse(day).unwrap().as_str(), day);
        }
        for day in [
            "2026-11-31",
            "2026-02-29",
            "1900-02-29",
            "2026-13-01",
            "2026-00-10",
            "2026-11-00",
            "2026-1-12",
            "20261112",
            "2026-11-12 ",
            "+026-11-12",
            "２026-11-12",
            "",
        ] {
            assert_eq!(Day::parse(day), None, "{day}");
        }
    }

    #[test]
    fn an_id_reads_back_as_it_is_written_and_nothing_else_reads() {
        let tagged = Id::of(&album("A", "One", Some(4242)));
        assert_eq!(tagged.to_string(), "r4242");
        let untagged = Id::of(&album("The Made-Up Artist", "Untitled", None));
        // The same folded artist and title, from any profile.
        assert_eq!(
            untagged,
            Id::of(&album("Made-Up Artist", "UNTITLED [Deluxe Edition]", None))
        );
        assert_ne!(untagged, Id::of(&album("Made-Up Artist", "Titled", None)));
        for id in [
            tagged,
            untagged,
            Id::Key(0),
            Id::Release(i64::MAX.cast_unsigned()),
        ] {
            assert_eq!(Id::parse(&id.to_string()), Some(id));
        }
        for text in [
            "r0",
            "r04242",
            "r+4242",
            "r9223372036854775808",
            "r",
            "4242",
            "k123",
            "k0123456789ABCDEF",
            "k0123456789abcdef0",
            "x0123456789abcdef",
            "",
        ] {
            assert_eq!(Id::parse(text), None, "{text}");
        }
    }
}
