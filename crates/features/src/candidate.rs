//! What the recommendations suggest (recommendations.md, Catalog connections): a master, so
//! the pressings of one album are one candidate, or a release of its own when it has no
//! master (ADR 0026). Connections are found on releases and folded into their candidate
//! here.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

use catalog::discogs::{Catalog, CatalogError, Entry};
use catalog::feedback::Kind;

/// A candidate's identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Candidate {
    /// A Discogs master, standing for all of its releases.
    Master(u64),
    /// A Discogs release whose `master_id` is 0.
    Release(u64),
}

impl Candidate {
    /// The candidate of `kind` with `id`, as the scores and the feedback store it.
    #[must_use]
    pub(crate) fn new(kind: Kind, id: u64) -> Self {
        match kind {
            Kind::Master => Self::Master(id),
            Kind::Release => Self::Release(id),
        }
    }

    /// The candidate's kind and id, as the scores and the feedback store it.
    #[must_use]
    pub(crate) fn kind_id(self) -> (Kind, u64) {
        match self {
            Self::Master(id) => (Kind::Master, id),
            Self::Release(id) => (Kind::Release, id),
        }
    }

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

    /// The candidate's title, year, artists and styles from the catalog. A master the masters
    /// dump lacks, which can happen, is shown by its first release; `None` for a release the
    /// catalog has no row for, or a master with neither row nor releases.
    ///
    /// # Errors
    ///
    /// When the catalog read fails.
    pub fn entry(self, catalog: &Catalog) -> Result<Option<Entry>, CatalogError> {
        match self {
            Self::Master(master_id) => match catalog.master_entry(master_id)? {
                Some(entry) => Ok(Some(entry)),
                None => catalog
                    .first_release(master_id)?
                    .map_or(Ok(None), |release_id| catalog.release_entry(release_id)),
            },
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

/// Each candidate's connections, from connections found on its releases: each a candidate
/// and what connects it (`K`, a credit, label or related artist the scores define). A
/// connection that several pressings of a master share counts once, so a master with many
/// reissues doesn't outscore one with a single pressing.
pub fn collapse<K: Eq + Hash>(
    connections: impl IntoIterator<Item = (Candidate, K)>,
) -> HashMap<Candidate, HashSet<K>> {
    let mut candidates: HashMap<Candidate, HashSet<K>> = HashMap::new();
    for (candidate, key) in connections {
        candidates.entry(candidate).or_default().insert(key);
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
