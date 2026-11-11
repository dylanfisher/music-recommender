//! What the recommendations suggest (recommendations.md, Catalog connections): a master, so
//! the pressings of one album are one candidate, or a release of its own when it has no
//! master (ADR 0026). Connections are found on releases and folded into their candidate
//! here.

use std::collections::HashMap;
use std::hash::Hash;

use catalog::discogs::{Catalog, CatalogError, Entry};

/// A candidate's identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Candidate {
    /// A Discogs master, standing for all of its releases.
    Master(u64),
    /// A Discogs release whose `master_id` is 0.
    Release(u64),
}

impl Candidate {
    /// The candidate release `release_id` is part of, or `None` when the catalog has no such
    /// release.
    ///
    /// # Errors
    ///
    /// When the catalog read fails.
    pub fn read(catalog: &Catalog, release_id: u64) -> Result<Option<Self>, CatalogError> {
        Ok(catalog
            .release_master(release_id)?
            .map(|master_id| Self::of(release_id, master_id)))
    }

    /// The candidate release `release_id` is part of, given its `master_id`, which Discogs
    /// writes as 0 for a release without a master.
    #[must_use]
    pub fn of(release_id: u64, master_id: u64) -> Self {
        match master_id {
            0 => Self::Release(release_id),
            master_id => Self::Master(master_id),
        }
    }

    /// The candidate's title, year, artists and styles from the catalog, `None` for a master
    /// or release it has no row for.
    ///
    /// # Errors
    ///
    /// When the catalog read fails.
    pub fn entry(self, catalog: &Catalog) -> Result<Option<Entry>, CatalogError> {
        match self {
            Self::Master(master_id) => catalog.master_entry(master_id),
            Self::Release(release_id) => catalog.release_entry(release_id),
        }
    }

    /// The release whose community data stands for the candidate: a master's
    /// `main_release`, and a release without a master itself. `None` when the catalog has no
    /// such master.
    ///
    /// Phase 6 makes it a master's most-owned version, where ratings gather, once the
    /// community data has the counts; this is the one place that choice is made.
    ///
    /// # Errors
    ///
    /// When the catalog read fails.
    pub fn representative(self, catalog: &Catalog) -> Result<Option<u64>, CatalogError> {
        match self {
            Self::Master(master_id) => catalog.main_release(master_id),
            Self::Release(release_id) => Ok(Some(release_id)),
        }
    }
}

/// Each candidate's connections, from connections found on its releases: each a candidate,
/// what connects it (`K`, a credit, label or style the scores define) and its weight. A
/// connection that several pressings of a master share counts once, at its greatest weight,
/// so a master with many reissues doesn't outscore one with a single pressing.
///
/// # Panics
///
/// When a weight is NaN, which no score can rank.
pub fn collapse<K: Eq + Hash>(
    connections: impl IntoIterator<Item = (Candidate, K, f64)>,
) -> HashMap<Candidate, HashMap<K, f64>> {
    let mut candidates: HashMap<Candidate, HashMap<K, f64>> = HashMap::new();
    for (candidate, key, weight) in connections {
        assert!(!weight.is_nan(), "a connection of {candidate:?} weighs NaN");
        let kept = candidates
            .entry(candidate)
            .or_default()
            .entry(key)
            .or_insert(weight);
        if weight > *kept {
            *kept = weight;
        }
    }
    candidates
}

#[cfg(test)]
mod tests {
    use super::Candidate;

    #[test]
    fn a_release_without_a_master_is_its_own_candidate() {
        assert_eq!(Candidate::of(7, 0), Candidate::Release(7));
        assert_eq!(Candidate::of(7, 3), Candidate::Master(3));
    }
}
