//! The community values in the score (recommendations.md, Community data in the score;
//! ADR 0032). Each candidate's catalog score is multiplied by a factor made from its
//! representative's reply: its rating shrunk toward its styles' mean by its votes, its
//! owners on a log scale and its want-to-have ratio, each against the list's candidates
//! with values. The list is then scaled to its best again. A candidate whose representative
//! has no values has a factor of 1, and a weight of 0 leaves the list as the catalog scored
//! it.

use std::collections::{BTreeSet, HashMap};

use catalog::community::{Community, Values};
use catalog::discogs::Entry;
use catalog::error::DatabaseError;

use crate::candidate::Candidate;
use crate::score::{self, ScoreError, Scored};

/// The votes a rating's prior counts as: a rating from 10 votes is halfway between its own
/// average and its styles' mean.
const PRIOR_VOTES: f64 = 10.0;
/// A style's mean is the prior once this many of the list's candidates with the style are
/// rated; below it, a style's mean is too few votes to go by, and the list's mean is used.
const STYLE_RATED: usize = 20;
/// What a star of the shrunk rating above the list's mean adds to the log of the factor:
/// at weight 1 it multiplies the score by about 1.65.
const RATING: f64 = 0.5;
/// What one more of `ln(1 + have)` than the list's mean adds: ten times the owners, about
/// 2.3 more, multiplies by about 1.26 at weight 1.
const OWNERS: f64 = 0.1;
/// What one more of `ln((1 + want) / (1 + have))` than the list's mean adds: twice the
/// list's ratio multiplies by about 1.19 at weight 1.
const WANTED: f64 = 0.25;

// A change to the constants or the rules here changes a stored list and needs a bump of
// `score::VERSION`.

/// Each of `scored`'s representatives, as a rebuild stores it: a master's most-owned version
/// when `community` has listed its versions, otherwise its main release (ADR 0030).
///
/// # Errors
///
/// When the community database fails.
pub(crate) fn representatives(
    scored: &[(Scored, Entry)],
    community: Option<&Community>,
) -> Result<HashMap<Candidate, Option<u64>>, DatabaseError> {
    scored
        .iter()
        .map(|(scored, entry)| {
            let most_owned = match (scored.candidate, community) {
                (Candidate::Master(id), Some(db)) => db.most_owned(id)?,
                _ => None,
            };
            Ok((
                scored.candidate,
                scored
                    .candidate
                    .representative(entry.main_release, most_owned),
            ))
        })
        .collect()
}

/// The values `community` holds for each candidate whose representative it fetched. A
/// candidate whose representative wasn't fetched, or answered 404, has none.
///
/// # Errors
///
/// When the community database fails.
pub(crate) fn values(
    representatives: &HashMap<Candidate, Option<u64>>,
    community: Option<&Community>,
) -> Result<HashMap<Candidate, Values>, DatabaseError> {
    let Some(db) = community else {
        return Ok(HashMap::new());
    };
    let fetched: HashMap<u64, Values> = db.values()?.into_iter().collect();
    Ok(representatives
        .iter()
        .filter_map(|(&candidate, &representative)| {
            let values = fetched.get(&representative?)?;
            Some((candidate, *values))
        })
        .collect())
}

/// Multiplies each of `scored`'s scores by its community factor at `weight`, from its
/// `values`, then scales the list to its best and sorts it again. With a weight of 0, or no
/// candidate with values, the list is left as it is.
///
/// # Errors
///
/// When a score comes out not above 0, which a finite weight can't make.
pub(crate) fn weigh(
    scored: &mut [(Scored, Entry)],
    values: &HashMap<Candidate, Values>,
    weight: f64,
) -> Result<(), ScoreError> {
    let Some(factors) = factors(scored, values, weight) else {
        return Ok(());
    };
    for ((candidate, _), factor) in scored.iter_mut().zip(factors) {
        candidate.score *= factor;
    }
    score::scale_to_best(scored)?;
    score::sort(scored);
    Ok(())
}

/// Each of `scored`'s community factors at `weight`, in its order: 1 for a candidate without
/// `values`. `None` with a weight of 0 or no candidate with values, which leave every
/// candidate's score as it is.
pub(crate) fn factors(
    scored: &[(Scored, Entry)],
    values: &HashMap<Candidate, Values>,
    weight: f64,
) -> Option<Vec<f64>> {
    if weight == 0.0 || values.is_empty() {
        return None;
    }
    let typical = Typical::new(scored, values);
    Some(
        scored
            .iter()
            .map(|(candidate, entry)| {
                values.get(&candidate.candidate).map_or(1.0, |values| {
                    (weight * typical.log_factor(values, &entry.styles)).exp()
                })
            })
            .collect(),
    )
}

/// What a candidate's values are measured against: the list's candidates with values.
struct Typical {
    /// The mean of every vote on the list's rated candidates, `None` when none has a vote.
    mean: Option<f64>,
    /// The mean of every vote on the rated candidates of each style with `STYLE_RATED` of
    /// them.
    styles: HashMap<u32, f64>,
    /// The mean of `owned` over the candidates with values.
    owned: f64,
    /// The mean of `sought` over the candidates with values.
    sought: f64,
}

/// The votes on some rated candidates.
#[derive(Default)]
struct Rated {
    /// How many candidates.
    candidates: usize,
    /// Their votes.
    votes: f64,
    /// Their averages times their votes, summed: the stars of every vote.
    stars: f64,
}

impl Rated {
    fn add(&mut self, values: &Values) {
        let votes = count(values.rating_count);
        self.candidates += 1;
        self.votes += votes;
        self.stars += votes * values.rating_average;
    }

    fn mean(&self) -> f64 {
        self.stars / self.votes
    }
}

impl Typical {
    fn new(scored: &[(Scored, Entry)], values: &HashMap<Candidate, Values>) -> Self {
        let mut all = Rated::default();
        let mut styles: HashMap<u32, Rated> = HashMap::new();
        let (mut owned_sum, mut sought_sum, mut with_values) = (0.0, 0.0, 0.0);
        for (candidate, entry) in scored {
            let Some(values) = values.get(&candidate.candidate) else {
                continue;
            };
            owned_sum += owned(values);
            sought_sum += sought(values);
            with_values += 1.0;
            if values.rating_count == 0 {
                continue;
            }
            all.add(values);
            for &style in entry.styles.iter().collect::<BTreeSet<_>>() {
                styles.entry(style).or_default().add(values);
            }
        }
        Self {
            mean: (all.candidates > 0).then(|| all.mean()),
            styles: styles
                .into_iter()
                .filter(|(_, rated)| rated.candidates >= STYLE_RATED)
                .map(|(style, rated)| (style, rated.mean()))
                .collect(),
            owned: owned_sum / with_values,
            sought: sought_sum / with_values,
        }
    }

    /// The log of the factor at weight 1 of a candidate with `values` and `styles`.
    fn log_factor(&self, values: &Values, styles: &[u32]) -> f64 {
        // With no votes on the list there is no mean to measure a rating against, and every
        // candidate's rating counts 0.
        let rating = self.mean.map_or(0.0, |mean| {
            let shrunk = (count(values.rating_count) * values.rating_average
                + PRIOR_VOTES * self.prior(styles, mean))
                / (count(values.rating_count) + PRIOR_VOTES);
            shrunk - mean
        });
        RATING * rating
            + OWNERS * (owned(values) - self.owned)
            + WANTED * (sought(values) - self.sought)
    }

    /// The rating a candidate of `styles` is shrunk toward: the mean of its styles' means,
    /// of those with enough rated candidates, or the list's `mean` when none has.
    fn prior(&self, styles: &[u32], mean: f64) -> f64 {
        let means: Vec<f64> = styles
            .iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter_map(|style| self.styles.get(style).copied())
            .collect();
        if means.is_empty() {
            mean
        } else {
            means.iter().sum::<f64>() / count(means.len() as u64)
        }
    }
}

/// The owners on a log scale.
fn owned(values: &Values) -> f64 {
    count(values.have).ln_1p()
}

/// The want-to-have ratio on a log scale, each count plus 1 so that none is 0.
fn sought(values: &Values) -> f64 {
    count(values.want).ln_1p() - count(values.have).ln_1p()
}

#[expect(
    clippy::cast_precision_loss,
    reason = "votes, owners and wants far below 2^52"
)]
fn count(n: u64) -> f64 {
    n as f64
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use catalog::community::Values;
    use catalog::discogs::Entry;

    use super::{STYLE_RATED, weigh};
    use crate::candidate::Candidate;
    use crate::score::Scored;

    /// A release candidate `id` at `score`, of `styles`.
    fn candidate(id: u64, score: f64, styles: &[u32]) -> (Scored, Entry) {
        (
            Scored {
                candidate: Candidate::Release(id),
                score,
                reasons: Vec::new(),
            },
            Entry {
                title: format!("Album {id}"),
                year: None,
                artists: Vec::new(),
                styles: styles.to_vec(),
                main_release: None,
            },
        )
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

    /// The candidates' ids in the list's order.
    fn order(scored: &[(Scored, Entry)]) -> Vec<u64> {
        scored
            .iter()
            .map(|(scored, _)| scored.candidate.kind_id().1)
            .collect()
    }

    /// Candidates 1 and 2 at an equal catalog score, and enough rated candidates 100 and on,
    /// below them, for their style's mean, all with the same owners and wants.
    fn rated_list() -> (Vec<(Scored, Entry)>, HashMap<Candidate, Values>) {
        let mut scored = vec![candidate(1, 1.0, &[7]), candidate(2, 1.0, &[7])];
        let mut known = HashMap::from([
            (Candidate::Release(1), values(5.0, 2, 300, 100)),
            (Candidate::Release(2), values(4.6, 800, 300, 100)),
        ]);
        for id in 100..100 + STYLE_RATED as u64 {
            scored.push(candidate(id, 0.5, &[7]));
            known.insert(Candidate::Release(id), values(3.8, 100, 300, 100));
        }
        (scored, known)
    }

    #[test]
    fn a_five_from_two_votes_ranks_below_a_four_point_six_from_800() {
        let (mut scored, known) = rated_list();
        // Equal scores: the catalog's order is by id, so 1 is ahead before the factor.
        assert_eq!(order(&scored)[..2], [1, 2]);
        weigh(&mut scored, &known, 1.0).unwrap();
        assert_eq!(order(&scored)[..2], [2, 1], "{scored:#?}");
        assert!((scored[0].0.score - 1.0).abs() < 1e-12, "{scored:#?}");
    }

    #[test]
    fn a_candidate_without_values_keeps_its_catalog_score_before_the_scaling() {
        let (mut scored, known) = rated_list();
        scored.push(candidate(50, 0.8, &[7]));
        scored.push(candidate(51, 0.4, &[]));
        weigh(&mut scored, &known, 1.0).unwrap();
        let score = |id| {
            scored
                .iter()
                .find(|(s, _)| s.candidate == Candidate::Release(id))
                .unwrap()
                .0
                .score
        };
        // Both are multiplied by 1 and then scaled with the rest, so they keep their ratio.
        assert!((score(50) / score(51) - 2.0).abs() < 1e-12, "{scored:#?}");
        // The best is 1, as every list's.
        assert!((scored[0].0.score - 1.0).abs() < 1e-12, "{scored:#?}");
    }

    #[test]
    fn a_list_without_values_is_the_catalog_s_score_for_score() {
        let (mut scored, _) = rated_list();
        let before: Vec<(u64, f64)> = scored
            .iter()
            .map(|(s, _)| (s.candidate.kind_id().1, s.score))
            .collect();
        weigh(&mut scored, &HashMap::new(), 1.0).unwrap();
        let after: Vec<(u64, f64)> = scored
            .iter()
            .map(|(s, _)| (s.candidate.kind_id().1, s.score))
            .collect();
        assert_eq!(after, before);
    }

    #[test]
    fn weight_0_leaves_the_list_as_the_catalog_scored_it() {
        let (mut scored, known) = rated_list();
        let before: Vec<(u64, f64)> = scored
            .iter()
            .map(|(s, _)| (s.candidate.kind_id().1, s.score))
            .collect();
        weigh(&mut scored, &known, 0.0).unwrap();
        let after: Vec<(u64, f64)> = scored
            .iter()
            .map(|(s, _)| (s.candidate.kind_id().1, s.score))
            .collect();
        assert_eq!(after, before);
    }

    #[test]
    fn owners_and_wants_raise_a_candidate_and_a_greater_weight_raises_it_more() {
        let lift = |weight: f64| {
            let mut scored = vec![candidate(1, 1.0, &[]), candidate(2, 0.9, &[])];
            let known = HashMap::from([
                (Candidate::Release(1), values(4.0, 50, 100, 20)),
                // Ten times the owners and more wants than owners.
                (Candidate::Release(2), values(4.0, 50, 1000, 1500)),
            ]);
            weigh(&mut scored, &known, weight).unwrap();
            assert_eq!(order(&scored), [2, 1], "{scored:#?}");
            scored[1].0.score
        };
        // 1 is now below 2, and further below at a greater weight.
        assert!(lift(2.0) < lift(1.0));
    }

    #[test]
    fn a_rating_is_shrunk_toward_its_style_s_mean_not_the_list_s() {
        // Style 7's candidates are rated 4.5, style 8's 3.0, so the list's mean is between.
        // Candidate 1, of style 7 with a 3.9 from 1 vote, is shrunk toward 4.5, above the
        // list's mean; candidate 2, of style 8 with the same, toward 3.0, below it.
        let mut scored = vec![candidate(1, 0.5, &[7]), candidate(2, 0.5, &[8])];
        let mut known = HashMap::from([
            (Candidate::Release(1), values(3.9, 1, 100, 50)),
            (Candidate::Release(2), values(3.9, 1, 100, 50)),
        ]);
        for id in 0..STYLE_RATED as u64 {
            scored.push(candidate(100 + id, 1.0, &[7]));
            known.insert(Candidate::Release(100 + id), values(4.5, 100, 100, 50));
            scored.push(candidate(200 + id, 1.0, &[8]));
            known.insert(Candidate::Release(200 + id), values(3.0, 100, 100, 50));
        }
        weigh(&mut scored, &known, 1.0).unwrap();
        let score = |id| {
            scored
                .iter()
                .find(|(s, _)| s.candidate == Candidate::Release(id))
                .unwrap()
                .0
                .score
        };
        assert!(score(1) > score(2), "{scored:#?}");
    }
}
