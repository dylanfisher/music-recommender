//! The orders a list's albums can be shown in (ADR 0033): by score, as the list is stored,
//! or by one of the community values among the first [`POOL`] albums by score that a page
//! would show. The pool keeps the order to the albums the library is close to: an album
//! with many owners and a weak connection never gets in.

use catalog::community::{Stored, Values};

use crate::community_factor::{PRIOR_VOTES, count};

/// How many albums, the first by score after the filters, the feedback and the caps, a sort
/// by a community value reorders. The sync fetches a list's top 200 without values soon after
/// it is asked for (ADR 0030), so most of a pool has values once the scrape runs.
pub const POOL: usize = 300;

/// The order to show a list's albums in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Sort {
    /// Best score first: the whole list, as stored.
    #[default]
    Score,
    /// The highest rating first, shrunk toward the pool's mean by its votes.
    Rating,
    /// The most owners first.
    Have,
    /// The most wants first.
    Want,
    /// The most wants for each owner first.
    WantHave,
}

impl Sort {
    /// Every sort, in the order the contract lists them.
    pub const ALL: [Self; 5] = [
        Self::Score,
        Self::Rating,
        Self::Have,
        Self::Want,
        Self::WantHave,
    ];

    /// Its name in a request.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Score => "score",
            Self::Rating => "rating",
            Self::Have => "have",
            Self::Want => "want",
            Self::WantHave => "want_have",
        }
    }

    /// The sort named `name`, `None` for a name not listed.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|sort| sort.name() == name)
    }
}

/// The values a stored row holds: a fetched release's, or a failed one's from an earlier
/// fetch.
pub(crate) fn values(stored: &Stored) -> Option<Values> {
    match stored {
        Stored::Fetched { values, .. } => Some(*values),
        Stored::Failed { values, .. } => *values,
        Stored::NotQueued | Stored::Queued | Stored::Gone { .. } => None,
    }
}

/// The places of `pool`, albums in score order with their values, in the order `sort` shows
/// them: the highest value first, a tie in score order, and every album without values after
/// those with, in score order. `Sort::Score` leaves the score order as it is.
pub(crate) fn order(pool: &[Option<Values>], sort: Sort) -> Vec<usize> {
    if sort == Sort::Score {
        return (0..pool.len()).collect();
    }
    let mean = mean(pool);
    let keys: Vec<Option<f64>> = pool
        .iter()
        .map(|values| values.map(|values| key(&values, sort, mean)))
        .collect();
    let mut order: Vec<usize> = (0..pool.len()).collect();
    // A stable sort, so a tie keeps the score order.
    order.sort_by(|&a, &b| match (keys[a], keys[b]) {
        (Some(a), Some(b)) => b.total_cmp(&a),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    order
}

/// The mean of every vote on the pool's rated albums, `None` when none has a vote.
fn mean(pool: &[Option<Values>]) -> Option<f64> {
    let (votes, stars) = pool
        .iter()
        .flatten()
        .fold((0.0, 0.0), |(votes, stars), values| {
            let n = count(values.rating_count);
            (votes + n, stars + n * values.rating_average)
        });
    (votes > 0.0).then(|| stars / votes)
}

/// What `sort` orders an album with `values` by, the highest first. A rating is shrunk by
/// `PRIOR_VOTES` toward the pool's `mean`, as the score's is toward its styles' (ADR 0032),
/// so a 5.0 from 2 votes doesn't come before a 4.6 from 800. With no vote in the pool every
/// rating is the same.
fn key(values: &Values, sort: Sort, mean: Option<f64>) -> f64 {
    match sort {
        Sort::Score => unreachable!("the score order isn't sorted by a value"),
        Sort::Rating => mean.map_or(0.0, |mean| {
            let votes = count(values.rating_count);
            (votes * values.rating_average + PRIOR_VOTES * mean) / (votes + PRIOR_VOTES)
        }),
        Sort::Have => count(values.have),
        Sort::Want => count(values.want),
        Sort::WantHave => (count(values.want) + 1.0) / (count(values.have) + 1.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rated(rating_average: f64, rating_count: u64, have: u64, want: u64) -> Values {
        Values {
            rating_average,
            rating_count,
            have,
            want,
            num_for_sale: None,
            lowest_price: None,
        }
    }

    #[test]
    fn a_few_high_votes_come_after_many_good_ones() {
        let pool = [
            Some(rated(5.0, 2, 10, 5)),
            Some(rated(4.6, 800, 3000, 900)),
            None,
            Some(rated(3.0, 400, 50, 1)),
        ];
        assert_eq!(order(&pool, Sort::Rating), [1, 0, 3, 2]);
    }

    #[test]
    fn each_count_orders_the_most_first_and_albums_without_values_last() {
        let pool = [
            None,
            Some(rated(4.0, 10, 100, 10)),
            Some(rated(4.0, 10, 5, 20)),
            None,
            Some(rated(4.0, 10, 40, 40)),
        ];
        assert_eq!(order(&pool, Sort::Have), [1, 4, 2, 0, 3]);
        assert_eq!(order(&pool, Sort::Want), [4, 2, 1, 0, 3]);
        // 21/6, 41/41 and 11/101.
        assert_eq!(order(&pool, Sort::WantHave), [2, 4, 1, 0, 3]);
    }

    #[test]
    fn a_tie_and_the_score_sort_keep_the_score_order() {
        let pool = [Some(rated(4.0, 10, 7, 1)), None, Some(rated(4.0, 10, 7, 1))];
        assert_eq!(order(&pool, Sort::Have), [0, 2, 1]);
        assert_eq!(order(&pool, Sort::Score), [0, 1, 2]);
        // No votes in the pool: every rating is the same.
        let unrated = [Some(rated(0.0, 0, 1, 1)), Some(rated(0.0, 0, 9, 9))];
        assert_eq!(order(&unrated, Sort::Rating), [0, 1]);
    }

    #[test]
    fn every_name_reads_back() {
        for sort in Sort::ALL {
            assert_eq!(Sort::named(sort.name()), Some(sort));
        }
        assert_eq!(Sort::named("rank"), None);
    }
}
