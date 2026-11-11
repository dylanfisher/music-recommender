//! The evaluation (recommendations.md, Lists and evaluation; ADR 0027): a tenth of the tagged
//! albums hidden by a seed, the whole library's list scored in memory from the rest, and how
//! near its top the hidden ones come back. A change to a weight is judged by these numbers
//! first. It writes nothing: the stored list is `rescore`'s.
//!
//! The split is by candidate, so the pressings of one master are hidden or kept together; a
//! master kept through one of its pressings would be the library's, and no list could find
//! it. Each candidate's place comes from a hash of its id and the seed, so a rerun hides the
//! same ones, and an album added to the library moves at most one candidate across the line.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::hash::BuildHasher;
use std::time::Duration;

use catalog::discogs::{Catalog, CatalogError, Reading};
use config::Config;

use crate::candidate::Candidate;
use crate::clock::Clock;
use crate::connection::Connections;
use crate::library::Library;
use crate::list::ListError;
use crate::profile::{self, Profile, ProfileError};
use crate::score::{self, Scored};
use crate::seed::Seeds;

/// The seed `evaluate` splits by unless asked for another.
pub const SEED: u64 = 1;
/// One candidate in this many is hidden, rounded up.
const SHARE: usize = 10;
/// The tops of the list the hidden candidates are counted in.
const TOPS: [usize; 2] = [100, 500];

/// The tagged albums' candidates and the ones a seed hides.
#[derive(Debug)]
pub struct Split {
    /// How many candidates the tagged albums are, leaving out compilations of Various
    /// Artists, which no list suggests.
    pub pool: usize,
    /// The hidden ones, a tenth of the pool rounded up.
    pub hidden: HashSet<Candidate>,
}

/// The list scored from the albums a split keeps, and where the hidden ones are in it.
#[derive(Debug)]
pub struct Ranked {
    /// The profile without the hidden candidates' albums: the seeds and the exclusions.
    pub kept: Profile,
    /// How many seeds the kept albums are.
    pub seeds: usize,
    /// The list, best first.
    pub list: Vec<Scored>,
    /// The hidden candidates, in the order of their ids.
    pub hidden: Vec<Hidden>,
}

/// A hidden candidate and where the list has it.
#[derive(Debug)]
pub struct Hidden {
    /// The candidate.
    pub candidate: Candidate,
    /// How many profile albums are tagged with its releases.
    pub albums: usize,
    /// Its rank in the list from 1, `None` when it isn't in it.
    pub rank: Option<usize>,
    /// Whether a kept album has its artist and title, so the list can't have it.
    pub named: bool,
}

/// What `evaluate` prints.
#[derive(Debug)]
pub struct Report {
    seed: u64,
    pool: usize,
    hidden: usize,
    albums: usize,
    /// The hidden candidates a kept album's artist and title exclude, left out of the counts.
    named: usize,
    seeds: usize,
    candidates: usize,
    counts: Counts,
    took: Duration,
}

/// How near the top the hidden candidates come back.
#[derive(Debug, PartialEq, Eq)]
struct Counts {
    /// How many were counted.
    of: usize,
    /// How many are in each of `TOPS`.
    tops: [usize; 2],
    /// How many are anywhere in the list.
    listed: usize,
    /// The median rank of those, the lower of the two middle ones for an even count.
    median: Option<usize>,
}

impl Counts {
    /// The counts of hidden candidates at `ranks`, `None` for one the list doesn't have.
    fn new(ranks: &[Option<usize>]) -> Self {
        let mut listed: Vec<usize> = ranks.iter().flatten().copied().collect();
        listed.sort_unstable();
        Self {
            of: ranks.len(),
            tops: TOPS.map(|top| listed.iter().filter(|&&rank| rank <= top).count()),
            listed: listed.len(),
            median: (!listed.is_empty()).then(|| listed[(listed.len() - 1) / 2]),
        }
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let plural = |n: usize| if n == 1 { "" } else { "s" };
        writeln!(
            f,
            "the split of seed {}: {} of the tagged albums' {} candidates hidden, {} album{}",
            self.seed,
            self.hidden,
            self.pool,
            self.albums,
            plural(self.albums)
        )?;
        writeln!(
            f,
            "left out: {} of them, which a kept album's artist and title exclude",
            self.named
        )?;
        writeln!(
            f,
            "scored from {} kept seeds: {} candidates",
            self.seeds, self.candidates
        )?;
        let counts = &self.counts;
        let median = counts
            .median
            .map_or_else(|| "none".to_string(), |rank| rank.to_string());
        writeln!(
            f,
            "found: {} of {} in the top {}, {} in the top {}, {} anywhere, median rank {median}",
            counts.tops[0], counts.of, TOPS[0], counts.tops[1], TOPS[1], counts.listed
        )?;
        write!(f, "took {:.1} s", self.took.as_secs_f64())
    }
}

/// Why the evaluation can't run.
#[derive(Debug)]
pub enum EvaluateError {
    /// There is no catalog to score from.
    NoCatalog,
    /// The catalog's tables are another version's.
    OtherCatalog,
    /// No profile has been kept.
    NoProfile,
    /// No album of the kept profile is tagged with a release the catalog has.
    NoTagged,
    /// The catalog failed.
    Catalog(CatalogError),
    /// The kept profile can't be read.
    Profile(ProfileError),
}

impl fmt::Display for EvaluateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // The lists' words, which say what to do.
            Self::NoCatalog => ListError::NoCatalog.fmt(f),
            Self::OtherCatalog => ListError::OtherCatalog.fmt(f),
            Self::NoProfile => ListError::NoProfile.fmt(f),
            Self::NoTagged => f.write_str(
                "no album of the kept profile is tagged with a release the catalog has, so \
                 there is nothing to hide; the player's Discogs tagger tags them",
            ),
            Self::Catalog(error) => error.fmt(f),
            Self::Profile(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for EvaluateError {}

impl From<CatalogError> for EvaluateError {
    fn from(error: CatalogError) -> Self {
        Self::Catalog(error)
    }
}

impl From<ProfileError> for EvaluateError {
    fn from(error: ProfileError) -> Self {
        Self::Profile(error)
    }
}

/// The evaluation of the kept profile in `config`'s data directory against its catalog,
/// hiding the candidates `seed` picks. Reads both and writes nothing.
///
/// # Errors
///
/// When there is no catalog or kept profile, no album is tagged with a release the catalog
/// has, or a read fails.
pub fn run(config: &Config, clock: &dyn Clock, seed: u64) -> Result<Report, EvaluateError> {
    let started = clock.now();
    let data_dir = &config.data_dir;
    let catalog = match Catalog::open_read_only(data_dir)? {
        Reading::Missing => return Err(EvaluateError::NoCatalog),
        Reading::OtherShape => return Err(EvaluateError::OtherCatalog),
        Reading::Open(catalog) => catalog,
    };
    let profile = profile::kept(data_dir)?.ok_or(EvaluateError::NoProfile)?;
    let split = split(&profile, &catalog, seed)?;
    if split.pool == 0 {
        return Err(EvaluateError::NoTagged);
    }
    let ranked = rank(&profile, &catalog, &split.hidden)?;
    let counted: Vec<Option<usize>> = ranked
        .hidden
        .iter()
        .filter(|hidden| !hidden.named)
        .map(|hidden| hidden.rank)
        .collect();
    Ok(Report {
        seed,
        pool: split.pool,
        hidden: ranked.hidden.len(),
        albums: ranked.hidden.iter().map(|hidden| hidden.albums).sum(),
        named: ranked.hidden.len() - counted.len(),
        seeds: ranked.seeds,
        candidates: ranked.list.len(),
        counts: Counts::new(&counted),
        // A clock set back while it ran reads as no time: the time is for reading, and
        // the numbers above stand without it.
        took: clock
            .now()
            .duration_since(started)
            .unwrap_or(Duration::ZERO),
    })
}

/// The candidates of `profile`'s tagged albums that `seed` hides: a tenth, rounded up, of
/// those the catalog has, compilations of Various Artists left out.
///
/// # Errors
///
/// When a catalog read fails.
pub fn split(profile: &Profile, catalog: &Catalog, seed: u64) -> Result<Split, CatalogError> {
    let mut pool = Vec::new();
    let mut taken = HashSet::new();
    for album in &profile.albums {
        let Some(release_id) = album.discogs_release_id else {
            continue;
        };
        let Some(candidate) = Candidate::read(catalog, release_id)? else {
            continue;
        };
        if !taken.insert(candidate) {
            continue;
        }
        if candidate
            .entry(catalog)?
            .is_some_and(|entry| score::is_various(&entry))
        {
            continue;
        }
        pool.push(candidate);
    }
    pool.sort_by_key(|&candidate| (place(seed, candidate), score::order(candidate)));
    let hidden = pool
        .iter()
        .take(pool.len().div_ceil(SHARE))
        .copied()
        .collect();
    Ok(Split {
        pool: pool.len(),
        hidden,
    })
}

/// The list scored from `profile` without the albums tagged with a release of a `hidden`
/// candidate, and where it ranks each of those. The kept albums are the seeds and the
/// exclusions both, so a hidden album neither seeds the list nor leaves itself out of it.
///
/// # Errors
///
/// When a catalog read fails.
pub fn rank<S: BuildHasher>(
    profile: &Profile,
    catalog: &Catalog,
    hidden: &HashSet<Candidate, S>,
) -> Result<Ranked, CatalogError> {
    let mut albums: HashMap<Candidate, usize> = hidden.iter().map(|&c| (c, 0)).collect();
    let mut kept = Vec::new();
    for album in &profile.albums {
        let candidate = match album.discogs_release_id {
            Some(release_id) => Candidate::read(catalog, release_id)?,
            None => None,
        };
        match candidate.and_then(|candidate| albums.get_mut(&candidate)) {
            Some(count) => *count += 1,
            None => kept.push(album.clone()),
        }
    }
    let kept = Profile { albums: kept };
    let seeds = Seeds::read(&kept, catalog)?;
    let connections = Connections::read(catalog, &seeds)?;
    let library = Library::new(&kept, catalog)?;
    let list = score::score(&seeds, &connections, &library);
    let ranks: HashMap<Candidate, usize> = list
        .iter()
        .enumerate()
        .map(|(index, scored)| (scored.candidate, index + 1))
        .collect();
    let mut hidden = albums
        .into_iter()
        .map(|(candidate, albums)| {
            Ok(Hidden {
                candidate,
                albums,
                rank: ranks.get(&candidate).copied(),
                named: candidate
                    .entry(catalog)?
                    .is_some_and(|entry| score::has_name(&library, &entry)),
            })
        })
        .collect::<Result<Vec<Hidden>, CatalogError>>()?;
    hidden.sort_by_key(|hidden| score::order(hidden.candidate));
    Ok(Ranked {
        kept,
        seeds: seeds.seeds.len(),
        list,
        hidden,
    })
}

/// Where `seed` places `candidate` in the split: a hash of both, so a candidate's place
/// doesn't depend on which others the library has.
fn place(seed: u64, candidate: Candidate) -> u64 {
    let (kind, id) = score::order(candidate);
    mix(mix(seed) ^ mix(id.wrapping_mul(2) | u64::from(kind)))
}

/// `SplitMix64`'s finalizer: every bit of the result depends on every bit of `z`. Written
/// here, a few lines, rather than taken from a crate; std's hashers may change between
/// releases, and a split has to stay the same.
fn mix(z: u64) -> u64 {
    let z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    let z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{Counts, Report};

    #[test]
    fn the_counts_are_the_tops_the_list_and_the_median_of_the_listed() {
        let counts = Counts::new(&[Some(1), Some(150), Some(600), None, Some(90)]);
        assert_eq!(
            counts,
            Counts {
                of: 5,
                tops: [2, 3],
                listed: 4,
                median: Some(90),
            }
        );
        assert_eq!(Counts::new(&[None]).median, None);
        assert_eq!(Counts::new(&[Some(7), Some(3), Some(5)]).median, Some(5));
    }

    #[test]
    fn the_report_says_each_number() {
        let report = Report {
            seed: 4,
            pool: 3952,
            hidden: 396,
            albums: 401,
            named: 2,
            seeds: 3551,
            candidates: 120_000,
            counts: Counts::new(&[Some(1), Some(150), None]),
            took: Duration::from_millis(1250),
        };
        assert_eq!(
            report.to_string(),
            "the split of seed 4: 396 of the tagged albums' 3952 candidates hidden, 401 albums\n\
             left out: 2 of them, which a kept album's artist and title exclude\n\
             scored from 3551 kept seeds: 120000 candidates\n\
             found: 1 of 3 in the top 100, 2 in the top 500, 2 anywhere, median rank 1\n\
             took 1.2 s"
        );
    }
}
