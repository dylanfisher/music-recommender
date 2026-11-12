//! The candidates' ratings by priority (ADR 0030): a release request for the representative
//! of each candidate of the stored list, a batch at a time, the next batch worked out when
//! the one before is fetched, so each starts from every reply before it.
//!
//! A candidate's priority is its catalog score, times a prior from how many own it as the
//! sweep listed it, times how its neighbors' replies went: candidates sharing an artist or
//! the label with well-rated, well-owned replies rise, and those whose neighbors have almost
//! no owners or votes sink. Every candidate queued has no rating yet, so how uncertain its
//! rating is is the same for all of them and leaves the order as it is: a fetched candidate
//! isn't queued again, and its value comes back by the refresh.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use catalog::community::{Community, ListedKind, Request, Values};
use catalog::error::DatabaseError;
use catalog::score::{Filter, Head, Scores};

use crate::candidate::Candidate;
use crate::sync::SyncError;

/// How many candidates a batch queues: about 17 minutes of requests, after which the
/// priorities are worked out again from the replies.
const BATCH: usize = 1000;

/// The owners a candidate no swept list names is taken to have, for its prior: a guess
/// below most listed releases' counts, so a candidate known to be owned goes first.
const UNKNOWN_HAVE: u64 = 10;

/// A reply is well rated and well owned with at least this average from at least this many
/// votes, and at least this many owners...
const WELL_RATED: f64 = 4.0;
const WELL_VOTED: u64 = 5;
const WELL_OWNED: u64 = 50;

/// ...and dead with fewer owners and fewer votes than these.
const FEW_OWNERS: u64 = 5;
const FEW_VOTES: u64 = 2;

/// What a well-rated, well-owned reply and a dead one count for in their neighbors'
/// priorities, against 1 for any other.
const RAISE: f64 = 2.0;
const SINK: f64 = 0.25;

/// The next batch of release requests, the highest priority first: the representatives of
/// the stored list's candidates that were never queued, at most `BATCH`. Empty when every
/// candidate with a representative has been queued.
///
/// # Errors
///
/// When the community database or the scores fail.
pub(crate) fn next_batch(
    scores: &Scores,
    db: &Community,
) -> Result<Vec<(Request, f64)>, SyncError> {
    let values: HashMap<u64, Values> = db.values()?.into_iter().collect();
    let queued: HashSet<u64> = db
        .queued_releases()?
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    let mut failed = None;
    let mut clusters = Clusters::default();
    scores.walk(
        &Filter::default(),
        false,
        &mut |head| match representative(db, &head) {
            Ok(Some(release)) => {
                if let Some(values) = values.get(&release) {
                    clusters.add(&head, values);
                }
                true
            }
            Ok(None) => true,
            Err(error) => {
                failed = Some(error);
                false
            }
        },
    )?;
    let mut batch = Vec::new();
    scores.walk(&Filter::default(), false, &mut |head| {
        let priced = representative(db, &head).and_then(|release| match release {
            Some(release) if !queued.contains(&release) => {
                let have = have(db, &head, release)?;
                Ok(Some((
                    release,
                    head.score * prior(have) * clusters.factor(&head),
                )))
            }
            _ => Ok(None),
        });
        match priced {
            Ok(priced) => {
                batch.extend(priced);
                true
            }
            Err(error) => {
                failed = Some(error);
                false
            }
        }
    })?;
    if let Some(error) = failed {
        return Err(error.into());
    }
    Ok(best(batch, BATCH)
        .into_iter()
        .map(|(release, priority)| (Request::release(release), priority))
        .collect())
}

/// The candidate's representative as the next rebuild will store it: the most-owned version
/// the sync has listed for a master, or the representative stored.
pub(crate) fn representative(db: &Community, head: &Head) -> Result<Option<u64>, DatabaseError> {
    let candidate = Candidate::new(head.kind, head.id);
    let most_owned = match candidate {
        Candidate::Master(id) => db.most_owned(id)?,
        Candidate::Release(_) => None,
    };
    Ok(candidate.representative(head.representative, most_owned))
}

/// The owners the sweep listed for the candidate: its representative's, or a master's own as
/// an artist's releases list it.
fn have(db: &Community, head: &Head, release: u64) -> Result<Option<u64>, DatabaseError> {
    if let Some(have) = db.listed(ListedKind::Release, release)? {
        return Ok(Some(have));
    }
    match Candidate::new(head.kind, head.id) {
        Candidate::Master(id) => db.listed(ListedKind::Master, id),
        Candidate::Release(_) => Ok(None),
    }
}

/// The popularity prior: 1 and the log of the owners, so a release a thousand own counts
/// about twice one ten own, and a guess of `UNKNOWN_HAVE` for one no list names.
fn prior(have: Option<u64>) -> f64 {
    #[expect(clippy::cast_precision_loss, reason = "owner counts")]
    let have = have.unwrap_or(UNKNOWN_HAVE) as f64;
    1.0 + have.ln_1p()
}

/// What a reply counts for in its neighbors' priorities.
fn verdict(values: &Values) -> f64 {
    if values.rating_average >= WELL_RATED
        && values.rating_count >= WELL_VOTED
        && values.have >= WELL_OWNED
    {
        RAISE
    } else if values.have < FEW_OWNERS && values.rating_count < FEW_VOTES {
        SINK
    } else {
        1.0
    }
}

/// What candidates are neighbors by: each of their artists, and their label.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Neighbor {
    Artist(u64),
    Label(u64),
}

/// The verdicts of the fetched candidates, summed for each artist and label with how many.
#[derive(Default)]
struct Clusters(HashMap<Neighbor, (f64, u32)>);

impl Clusters {
    fn add(&mut self, head: &Head, values: &Values) {
        let verdict = verdict(values);
        for neighbor in neighbors(head) {
            let (sum, count) = self.0.entry(neighbor).or_default();
            *sum += verdict;
            *count += 1;
        }
    }

    /// The mean verdict of the candidate's clusters that have fetched candidates, each the
    /// mean of its own; 1 when none has.
    fn factor(&self, head: &Head) -> f64 {
        let means: Vec<f64> = neighbors(head)
            .filter_map(|neighbor| self.0.get(&neighbor))
            .map(|&(sum, count)| sum / f64::from(count))
            .collect();
        if means.is_empty() {
            1.0
        } else {
            #[expect(clippy::cast_precision_loss, reason = "a candidate's few neighbors")]
            let count = means.len() as f64;
            means.iter().sum::<f64>() / count
        }
    }
}

fn neighbors(head: &Head) -> impl Iterator<Item = Neighbor> + '_ {
    head.artists
        .iter()
        .map(|artist| Neighbor::Artist(artist.id))
        .chain(head.label_id.map(Neighbor::Label))
}

/// The `size` items of the highest priority, highest first, equal ones by id.
fn best(mut items: Vec<(u64, f64)>, size: usize) -> Vec<(u64, f64)> {
    items.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    items.truncate(size);
    items
}

#[cfg(test)]
mod tests {
    use catalog::community::Values;
    use catalog::feedback::Kind;
    use catalog::score::{Head, Named};

    use super::{Clusters, best, prior};

    /// A release candidate by `artists` on `label`.
    fn head(id: u64, artists: &[u64], label: Option<u64>) -> Head {
        Head {
            rank: id,
            kind: Kind::Release,
            id,
            representative: Some(id),
            score: 1.0,
            label_id: label,
            artists: artists.iter().map(|&id| Named::new(id, None)).collect(),
        }
    }

    const WELL: Values = Values {
        rating_average: 4.4,
        rating_count: 120,
        have: 2000,
        want: 500,
        num_for_sale: None,
        lowest_price: None,
    };

    const DEAD: Values = Values {
        rating_average: 0.0,
        rating_count: 1,
        have: 3,
        want: 0,
        ..WELL
    };

    const MIDDLING: Values = Values {
        rating_average: 3.2,
        rating_count: 9,
        have: 40,
        ..WELL
    };

    #[test]
    fn a_well_rated_reply_raises_its_neighbors_and_a_dead_cluster_sinks() {
        let mut clusters = Clusters::default();
        clusters.add(&head(1, &[10], Some(100)), &WELL);
        clusters.add(&head(2, &[20], Some(200)), &DEAD);
        clusters.add(&head(3, &[20], None), &DEAD);
        clusters.add(&head(4, &[40], Some(400)), &MIDDLING);
        let factor = |artists: &[u64], label| clusters.factor(&head(9, artists, label));
        // By the well-rated reply's artist, or on its label.
        assert!((factor(&[10], None) - 2.0).abs() < 1e-9);
        assert!((factor(&[30], Some(100)) - 2.0).abs() < 1e-9);
        // By the dead cluster's artist, or on its label.
        assert!((factor(&[20], None) - 0.25).abs() < 1e-9);
        assert!((factor(&[30], Some(200)) - 0.25).abs() < 1e-9);
        // Both: the mean of the two.
        assert!((factor(&[10], Some(200)) - 1.125).abs() < 1e-9);
        // A middling reply, or no fetched neighbor, leaves it as it was.
        assert!((factor(&[40], Some(400)) - 1.0).abs() < 1e-9);
        assert!((factor(&[30], Some(300)) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_batch_is_the_highest_priorities_and_the_next_starts_where_it_ends() {
        let items = vec![(1, 0.5), (2, 0.9), (3, 0.5), (4, 0.1)];
        assert_eq!(best(items.clone(), 2), [(2, 0.9), (1, 0.5)]);
        let rest: Vec<_> = items.into_iter().filter(|(id, _)| *id > 2).collect();
        assert_eq!(best(rest, 2), [(3, 0.5), (4, 0.1)]);
    }

    #[test]
    fn the_prior_grows_with_the_log_of_the_owners() {
        assert!((prior(Some(0)) - 1.0).abs() < f64::EPSILON);
        assert!(prior(Some(1000)) > prior(None));
        assert!(prior(Some(1000)) < 3.0 * prior(Some(10)));
    }
}
