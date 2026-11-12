//! The evaluation (recommendations.md, Lists and evaluation; ADR 0027): a tenth of the tagged
//! albums hidden by a seed, the whole library's list scored in memory from the rest, and how
//! near its top the hidden ones come back, by the catalog score alone and with the community
//! values weighed in (ADR 0032). A change to a weight is judged by these numbers first. It
//! writes nothing: the stored list is `rescore`'s.
//!
//! The split is by candidate, so the pressings of one master are hidden or kept together; a
//! master kept through one of its pressings would be the library's, and no list could find
//! it. Each candidate's place comes from a hash of its id and the seed, so a rerun hides the
//! same ones, and an album added to the library moves at most one candidate across the line.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::hash::BuildHasher;
use std::time::Duration;

use catalog::community::Community;
use catalog::discogs::{Catalog, CatalogError, Reading};
use catalog::error::DatabaseError;
use config::Config;

use crate::candidate::Candidate;
use crate::clock::Clock;
use crate::community_factor;
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
    /// The list by the catalog score alone, best first.
    pub list: Vec<Scored>,
    /// The list's candidates with the community values weighed in, best first.
    pub weighed: Vec<Candidate>,
    /// How many of the list's candidates have community values.
    pub with_values: usize,
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
    /// Its rank in the weighed list from 1, `None` when it isn't in it.
    pub weighed_rank: Option<usize>,
    /// Whether it is in the list with community values.
    pub has_values: bool,
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
    /// The community values' weight.
    weight: f64,
    /// How many of the counted hidden candidates have community values.
    hidden_with_values: usize,
    /// How many of the list's candidates have them.
    with_values: usize,
    /// The counts in the list with the community values weighed in.
    weighed: Counts,
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
        writeln!(f, "found: {}", self.counts)?;
        writeln!(
            f,
            "community values at weight {}: {} of the {} hidden, {} of the {} candidates",
            self.weight, self.hidden_with_values, self.counts.of, self.with_values, self.candidates
        )?;
        writeln!(f, "found with them: {}", self.weighed)?;
        write!(f, "took {:.1} s", self.took.as_secs_f64())
    }
}

impl fmt::Display for Counts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let median = self
            .median
            .map_or_else(|| "none".to_string(), |rank| rank.to_string());
        write!(
            f,
            "{} of {} in the top {}, {} in the top {}, {} anywhere, median rank {median}",
            self.tops[0], self.of, TOPS[0], self.tops[1], TOPS[1], self.listed
        )
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
    /// The community database failed.
    Database(DatabaseError),
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
            Self::Database(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for EvaluateError {}

impl From<DatabaseError> for EvaluateError {
    fn from(error: DatabaseError) -> Self {
        Self::Database(error)
    }
}

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
/// hiding the candidates `seed` picks, by the catalog score and with the community values
/// there weighed in at the config's weight. Reads them and writes nothing.
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
    // A database of another shape holds nothing to read, as for the rebuild.
    let community = match Community::open_read_only(data_dir)? {
        Reading::Open(db) => Some(db),
        Reading::Missing | Reading::OtherShape => None,
    };
    let weight = config.community_weight;
    let ranked = rank(
        &profile,
        &catalog,
        &split.hidden,
        community.as_ref(),
        weight,
    )?;
    let counted: Vec<&Hidden> = ranked
        .hidden
        .iter()
        .filter(|hidden| !hidden.named)
        .collect();
    let ranks = |rank: fn(&Hidden) -> Option<usize>| -> Vec<Option<usize>> {
        counted.iter().map(|&hidden| rank(hidden)).collect()
    };
    Ok(Report {
        seed,
        pool: split.pool,
        hidden: ranked.hidden.len(),
        albums: ranked.hidden.iter().map(|hidden| hidden.albums).sum(),
        named: ranked.hidden.len() - counted.len(),
        seeds: ranked.seeds,
        candidates: ranked.list.len(),
        counts: Counts::new(&ranks(|hidden| hidden.rank)),
        weight,
        hidden_with_values: counted.iter().filter(|hidden| hidden.has_values).count(),
        with_values: ranked.with_values,
        weighed: Counts::new(&ranks(|hidden| hidden.weighed_rank)),
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
/// candidate, and where it ranks each of those, by the catalog score and with the values of
/// `community` weighed in at `weight`. The kept albums are the seeds and the exclusions
/// both, so a hidden album neither seeds the list nor leaves itself out of it.
///
/// # Errors
///
/// When a catalog or community read fails.
pub fn rank<S: BuildHasher>(
    profile: &Profile,
    catalog: &Catalog,
    hidden: &HashSet<Candidate, S>,
    community: Option<&Community>,
    weight: f64,
) -> Result<Ranked, EvaluateError> {
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
    let inconsistent = |error: score::ScoreError| catalog.inconsistent(error.to_string());
    let scored = score::score(&seeds, connections, &library).map_err(inconsistent)?;
    let representatives = community_factor::representatives(&scored, community)?;
    let values = community_factor::values(&representatives, community)?;
    // The order alone is compared, so the weighed scores are neither scaled nor kept with
    // their reasons: a second list of the whole would double the memory.
    let factors = community_factor::factors(&scored, &values, weight)
        .unwrap_or_else(|| vec![1.0; scored.len()]);
    let mut weighed: Vec<(Scored, ())> = scored
        .iter()
        .zip(factors)
        .map(|((scored, _), factor)| {
            let weighed = Scored {
                candidate: scored.candidate,
                score: scored.score * factor,
                reasons: Vec::new(),
            };
            (weighed, ())
        })
        .collect();
    score::sort(&mut weighed);
    let weighed: Vec<Candidate> = weighed.into_iter().map(|(s, ())| s.candidate).collect();
    let list: Vec<Scored> = scored.into_iter().map(|(scored, _)| scored).collect();
    let ranks = ranks_in(list.iter().map(|scored| scored.candidate));
    let weighed_ranks = ranks_in(weighed.iter().copied());
    let mut hidden = albums
        .into_iter()
        .map(|(candidate, albums)| {
            Ok(Hidden {
                candidate,
                albums,
                rank: ranks.get(&candidate).copied(),
                weighed_rank: weighed_ranks.get(&candidate).copied(),
                has_values: values.contains_key(&candidate),
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
        with_values: list
            .iter()
            .filter(|scored| values.contains_key(&scored.candidate))
            .count(),
        list,
        weighed,
        hidden,
    })
}

/// Each candidate of `list`, best first, and its rank there, from 1.
fn ranks_in(list: impl Iterator<Item = Candidate>) -> HashMap<Candidate, usize> {
    list.zip(1..).collect()
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
            weight: 0.5,
            hidden_with_values: 2,
            with_values: 30_000,
            weighed: Counts::new(&[Some(1), Some(90), Some(700)]),
            took: Duration::from_millis(1250),
        };
        assert_eq!(
            report.to_string(),
            "the split of seed 4: 396 of the tagged albums' 3952 candidates hidden, 401 albums\n\
             left out: 2 of them, which a kept album's artist and title exclude\n\
             scored from 3551 kept seeds: 120000 candidates\n\
             found: 1 of 3 in the top 100, 2 in the top 500, 2 anywhere, median rank 1\n\
             community values at weight 0.5: 2 of the 3 hidden, 30000 of the 120000 candidates\n\
             found with them: 2 of 3 in the top 100, 2 in the top 500, 3 anywhere, median rank \
             90\n\
             took 1.2 s"
        );
    }
}
