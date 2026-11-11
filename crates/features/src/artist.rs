//! The artists of a list of candidates (ADR 0026, The lists): each scored by its best three
//! candidates, scaled to the best artist's, leaving out Various Artists and the library's
//! own artists, which are suggested only through albums of theirs the library doesn't have.
//! The whole library's roll-up is made when the scores are rebuilt and stored; a seed list's
//! is made when asked.

use std::collections::{HashMap, HashSet};

use catalog::discogs::{Catalog, CatalogError, Entry};

use crate::library;
use crate::profile::Album as LibraryAlbum;
use crate::score::VARIOUS;

/// What an artist's best, second and third candidates add to its score.
const SHARES: [f64; 3] = [1.0, 0.25, 0.1];
/// The prototype's word that joins a featured artist to an album's artist.
const FEATURING: &str = " feat. ";

/// The keys an album artist's name matches by: the whole name folded, and each name it
/// joins with "feat.", as the prototype's lists matched.
pub(crate) fn artist_keys(name: &str) -> impl Iterator<Item = String> + '_ {
    let parts = name
        .contains(FEATURING)
        .then(|| name.split(FEATURING))
        .into_iter()
        .flatten();
    std::iter::once(name)
        .chain(parts)
        .filter_map(library::artist_key)
}

/// The artists of some library albums: the Discogs artists of the releases they are tagged
/// with, and their own artist names folded.
#[derive(Debug, Default)]
pub(crate) struct Artists {
    ids: HashSet<u64>,
    names: HashSet<String>,
}

impl Artists {
    pub(crate) fn read<'a>(
        albums: impl Iterator<Item = &'a LibraryAlbum>,
        catalog: &Catalog,
    ) -> Result<Self, CatalogError> {
        let mut artists = Self::default();
        for album in albums {
            artists.names.extend(artist_keys(&album.artist));
            if let Some(release_id) = album.discogs_release_id
                && let Some(links) = catalog.links(release_id)?
            {
                artists
                    .ids
                    .extend(links.artists.into_iter().filter(|&id| id != VARIOUS));
            }
        }
        Ok(artists)
    }

    /// Whether the artist `id`, named `name`, is one of these.
    pub(crate) fn has(&self, id: u64, name: Option<&str>) -> bool {
        self.ids.contains(&id)
            || name
                .and_then(library::artist_key)
                .is_some_and(|key| self.names.contains(&key))
    }

    /// Whether any of `entry`'s artists is one of these.
    pub(crate) fn has_any(&self, entry: &Entry) -> bool {
        entry
            .artists
            .iter()
            .any(|(id, name)| self.has(*id, name.as_deref()))
    }
}

/// An artist of a roll-up.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Rolled {
    /// Its Discogs id.
    pub(crate) id: u64,
    /// Its name, `None` when the catalog has no row for it.
    pub(crate) name: Option<String>,
    /// Its best candidate's score with a quarter of its second's and a tenth of its third's,
    /// from 0 to 1 for the best artist.
    pub(crate) score: f64,
    /// The candidates its score counts, at most 3.
    pub(crate) albums: usize,
    /// The place, from 0, of its best candidate among the ones rolled up.
    pub(crate) best: usize,
}

/// Every artist of `candidates`, best first, each by its best three candidates so a large
/// catalog doesn't win on size, scaled to the best artist's score; `excluded` says which
/// artists, by id and name, are left out. The candidates are in rank order, a `None` having
/// no entry to read artists from.
pub(crate) fn roll_up<'a>(
    candidates: impl IntoIterator<Item = Option<(&'a Entry, f64)>>,
    excluded: &dyn Fn(u64, Option<&str>) -> bool,
) -> Vec<Rolled> {
    let mut artists: Vec<Rolled> = Vec::new();
    let mut by_id: HashMap<u64, usize> = HashMap::new();
    for (place, candidate) in candidates.into_iter().enumerate() {
        let Some((entry, score)) = candidate else {
            continue;
        };
        let mut seen = HashSet::new();
        for (id, name) in &entry.artists {
            if *id == VARIOUS || !seen.insert(*id) || excluded(*id, name.as_deref()) {
                continue;
            }
            let at = *by_id.entry(*id).or_insert_with(|| {
                artists.push(Rolled {
                    id: *id,
                    name: name.clone(),
                    score: 0.0,
                    albums: 0,
                    best: place,
                });
                artists.len() - 1
            });
            // `by_id` holds only indexes of `artists`, pushed beside them just above.
            let artist = &mut artists[at];
            if let Some(share) = SHARES.get(artist.albums) {
                artist.score += share * score;
                artist.albums += 1;
            }
        }
    }
    artists.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.id.cmp(&b.id)));
    if let Some(best) = artists.first().map(|artist| artist.score)
        && best > 0.0
    {
        for artist in &mut artists {
            artist.score /= best;
        }
    }
    artists
}

#[cfg(test)]
mod tests {
    use catalog::discogs::Entry;

    use super::{Rolled, roll_up};

    /// An entry by `artists`.
    fn by(artists: &[u64]) -> Entry {
        Entry {
            title: "Title".to_string(),
            year: None,
            artists: artists.iter().map(|&id| (id, None)).collect(),
            styles: Vec::new(),
            main_release: None,
        }
    }

    /// The roll-up of candidates by `artists`, each with its score, in the order given.
    fn rolled(
        candidates: &[(Vec<u64>, f64)],
        excluded: &dyn Fn(u64, Option<&str>) -> bool,
    ) -> Vec<Rolled> {
        let entries: Vec<Entry> = candidates.iter().map(|(artists, _)| by(artists)).collect();
        roll_up(
            entries
                .iter()
                .zip(candidates)
                .map(|(entry, (_, score))| Some((entry, *score))),
            excluded,
        )
    }

    #[test]
    fn an_artist_scores_its_best_album_with_a_quarter_and_a_tenth_of_the_next() {
        let artists = rolled(
            &[
                (vec![1], 1.0),
                (vec![2], 0.9),
                (vec![1], 0.5),
                (vec![1], 0.4),
                // A fourth counts nothing.
                (vec![1], 0.3),
                (vec![3], 0.2),
            ],
            &|id, _| id == 3,
        );
        let ids: Vec<u64> = artists.iter().map(|artist| artist.id).collect();
        assert_eq!(ids, [1, 2]);
        let first = 1.0 + 0.25 * 0.5 + 0.1 * 0.4;
        assert!((artists[0].score - 1.0).abs() < 1e-12);
        assert!((artists[1].score - 0.9 / first).abs() < 1e-12);
        assert_eq!(artists[0].albums, 3);
        // The best candidate is the first one read for each.
        assert_eq!((artists[0].best, artists[1].best), (0, 1));
    }

    #[test]
    fn an_artist_read_far_down_can_climb_into_the_first_places() {
        // Artist 3 starts at 0.79, under artist 2's 0.8. After 61 artists of 0.5 it gets
        // two albums of 0.5 more and passes artist 2: the whole list is read.
        let mut candidates = vec![(vec![1], 1.0), (vec![2], 0.8), (vec![3], 0.79)];
        candidates.extend((4..65).map(|id| (vec![id], 0.5)));
        candidates.push((vec![3], 0.5));
        candidates.push((vec![3], 0.5));
        let artists = rolled(&candidates, &|_, _| false);
        let ids: Vec<u64> = artists.iter().take(2).map(|artist| artist.id).collect();
        assert_eq!(ids, [1, 3]);
    }

    #[test]
    fn various_and_a_candidate_without_an_entry_are_left_out() {
        let entry = by(&[194, 5]);
        let artists = roll_up([None, Some((&entry, 1.0))], &|_, _| false);
        assert_eq!(artists.len(), 1);
        assert_eq!((artists[0].id, artists[0].best), (5, 1));
    }
}
