//! The library as the exclusion rules see it (recommendations.md, Lists and evaluation): the
//! Discogs release ids its albums are tagged with and their masters, and every album's folded
//! artist and title, tagged or not, so a candidate the library already has is never suggested
//! (ADR 0025, 0026).
//!
//! A candidate is the library's when its folded artist and title equal an album's, or one
//! title is a prefix of the other, of at least `MIN_PREFIX` characters, with one artist's
//! folded words a run of the other's, of at least `MIN_ARTIST` characters: "Made-Up Album"
//! against "Made-Up Album [Live Subtitle]". Albums by Various Artists or with no artist take
//! no part in either: a compilation's title alone doesn't say which one it is, so they are
//! excluded by release id only.

use std::collections::{HashMap, HashSet};

use primitives::fold;

use catalog::discogs::{Catalog, CatalogError};

use crate::candidate::Candidate;
use crate::profile::Profile;
use crate::query;

// A change to a constant here that changes a stored list needs a bump of `score::VERSION`.
/// The fewest characters a title prefix matches by.
const MIN_PREFIX: usize = 5;
/// The fewest characters an artist name matches inside another by.
const MIN_ARTIST: usize = 3;

/// The library's match data, built from a profile and the catalog.
#[derive(Debug, Default)]
pub struct Library {
    /// The Discogs release ids the library's albums are tagged with.
    pub release_ids: HashSet<u64>,
    /// The masters of the tagged releases, read from the catalog.
    master_ids: HashSet<u64>,
    /// Each album's folded artist and title.
    names: HashSet<(String, String)>,
    /// Each album's folded title, with the folded artist words of the albums that have it.
    titles: HashMap<String, HashSet<Vec<String>>>,
    /// Each proper prefix of an album's folded title of at least `MIN_PREFIX` characters,
    /// with the folded artist words of the albums whose titles start with it.
    prefixes: HashMap<String, HashSet<Vec<String>>>,
}

impl Library {
    /// The match data of `profile`'s albums, with the masters of their tagged releases read
    /// from `catalog`. A tagged release the catalog doesn't have, newer than its dumps or
    /// deleted from Discogs, still excludes its own id.
    ///
    /// # Errors
    ///
    /// When the catalog read fails.
    pub fn new(profile: &Profile, catalog: &Catalog) -> Result<Self, CatalogError> {
        let mut library = Self::of_profile(profile);
        for &release_id in &library.release_ids {
            if let Some(Candidate::Master(master_id)) = Candidate::read(catalog, release_id)? {
                library.master_ids.insert(master_id);
            }
        }
        Ok(library)
    }

    /// Whether `candidate`'s master or release is one the library's albums are tagged with,
    /// so a master is the library's when any of its pressings is.
    #[must_use]
    pub fn has_id(&self, candidate: Candidate) -> bool {
        match candidate {
            Candidate::Master(master_id) => self.master_ids.contains(&master_id),
            Candidate::Release(release_id) => self.release_ids.contains(&release_id),
        }
    }

    /// The match data of `profile`'s albums, without the masters.
    pub(crate) fn of_profile(profile: &Profile) -> Self {
        let mut library = Self::default();
        for album in &profile.albums {
            library.release_ids.extend(album.discogs_release_id);
            let Some(words) = artist_words(&album.artist) else {
                continue;
            };
            let title = fold::key(&album.title, true);
            library.names.insert((words.concat(), title.clone()));
            if title.is_empty() {
                continue;
            }
            for prefix in prefixes(&title) {
                library
                    .prefixes
                    .entry(prefix.to_string())
                    .or_default()
                    .insert(words.clone());
            }
            library.titles.entry(title).or_default().insert(words);
        }
        library
    }

    /// Whether a candidate credited to `artists`, titled `title`, is a library album by its
    /// folded names.
    #[must_use]
    pub(crate) fn has_name(&self, artists: &[&str], title: &str) -> bool {
        let words: Vec<Vec<String>> = artists
            .iter()
            .filter_map(|name| artist_words(name))
            .collect();
        let artists: Vec<String> = words.iter().map(|words| words.concat()).collect();
        let title = fold::key(title, true);
        if credit_keys(&artists).any(|artist| self.names.contains(&(artist, title.clone()))) {
            return true;
        }
        if title.is_empty() {
            return false;
        }
        // The artists of library albums whose title is this one, starts with it, or is a
        // prefix of it.
        let mut library_artists: HashSet<&Vec<String>> = HashSet::new();
        library_artists.extend(self.titles.get(&title).into_iter().flatten());
        if title.chars().count() >= MIN_PREFIX {
            library_artists.extend(self.prefixes.get(&title).into_iter().flatten());
        }
        for prefix in prefixes(&title) {
            library_artists.extend(self.titles.get(prefix).into_iter().flatten());
        }
        words.iter().any(|artist| {
            library_artists
                .iter()
                .any(|library_artist| inside(artist, library_artist))
        })
    }
}

/// The folded words of an album's artist, or `None` for Various Artists or no artist.
fn artist_words(name: &str) -> Option<Vec<String>> {
    let words = fold::words(name, false);
    (!words.is_empty() && !query::is_various(name)).then_some(words)
}

/// The folded key of an album's artist, or `None` for Various Artists or no artist.
pub(crate) fn artist_key(name: &str) -> Option<String> {
    artist_words(name).map(|words| words.concat())
}

/// The keys a credit of several artists matches by: each alone, and all of them run together
/// with and without "and", for a library "A & B" against a Discogs credit of A and B.
fn credit_keys(artists: &[String]) -> impl Iterator<Item = String> {
    let joined = (artists.len() > 1).then(|| [artists.concat(), artists.join("and")]);
    artists.iter().cloned().chain(joined.into_iter().flatten())
}

/// The proper prefixes of `key` of at least `MIN_PREFIX` characters, shortest first.
fn prefixes(key: &str) -> impl Iterator<Item = &str> {
    key.char_indices()
        .skip(MIN_PREFIX)
        .map(move |(end, _)| &key[..end])
}

/// Whether the folded words of the shorter of two artist names, of at least `MIN_ARTIST`
/// characters, are a run of the other's words. Whole words, since "air" is letters inside
/// "fairportconvention" but not one of its words.
fn inside(a: &[String], b: &[String]) -> bool {
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    short.concat().chars().count() >= MIN_ARTIST
        && long.windows(short.len()).any(|run| run == short)
}

#[cfg(test)]
mod tests {
    use super::Library;
    use crate::profile::{Album, Profile};

    fn album(artist: &str, title: &str, id: Option<u64>) -> Album {
        Album {
            artist: artist.to_string(),
            title: title.to_string(),
            track_count: 1,
            durations: vec![60],
            catalog_numbers: Vec::new(),
            barcodes: Vec::new(),
            discogs_release_id: id,
            plays: 0,
            loved: 0,
            rating: None,
            key: format!("{artist} {title}"),
            last_played: None,
            skips: 0,
        }
    }

    /// A library of untagged albums, each `(artist, title)`.
    fn untagged(albums: &[(&str, &str)]) -> Library {
        Library::of_profile(&Profile {
            albums: albums
                .iter()
                .map(|(artist, title)| album(artist, title, None))
                .collect(),
        })
    }

    #[test]
    fn keeps_the_tagged_release_ids() {
        let library = Library::of_profile(&Profile {
            albums: vec![
                album("Made-Up Artist", "Made-Up Album", Some(4242)),
                album("Made-Up Artist", "Untagged Album", None),
            ],
        });
        assert_eq!(library.release_ids, [4242].into());
        // A tagged album is excluded by its names too.
        assert!(library.has_name(&["Made-Up Artist"], "Made-Up Album"));
    }

    #[test]
    fn the_same_folded_artist_and_title_is_the_library_s() {
        let library = untagged(&[("The Made-Up Artist", "Made-Up Album (Remastered)")]);
        assert!(library.has_name(&["Made Up Artist (2)"], "made-up album"));
    }

    #[test]
    fn a_credit_of_several_artists_matches_them_joined() {
        let library = untagged(&[("Made-Up & Other", "Made-Up Album")]);
        assert!(library.has_name(&["Made-Up", "Other"], "Made-Up Album"));
    }

    #[test]
    fn a_title_with_a_disc_suffix_is_the_library_s() {
        let library = untagged(&[("Made-Up Artist", "Novelty Waves (CD2)")]);
        assert!(library.has_name(&["Made-Up Artist"], "Novelty Waves"));
        let library = untagged(&[("Made-Up Artist", "Novelty Waves")]);
        assert!(library.has_name(&["Made-Up Artist"], "Novelty Waves (Part 2)"));
    }

    #[test]
    fn a_title_prefix_with_one_artist_inside_the_other_is_the_library_s() {
        let library = untagged(&[("Made-Up Artist", "Live In Detroit [Made-Up Subtitle]")]);
        // The candidate's title is a prefix of the library's, and the library's artist is
        // inside the candidate's.
        assert!(library.has_name(&["Made-Up Artist Plays Another"], "Live In Detroit"));
        // And the other way round.
        let library = untagged(&[("Made-Up Artist Plays Another", "Live In Detroit")]);
        assert!(library.has_name(&["Made-Up Artist"], "Live In Detroit [Made-Up Subtitle]"));
    }

    #[test]
    fn a_title_prefix_shorter_than_five_characters_doesn_t_match() {
        let library = untagged(&[("Made-Up Artist", "Wave Of Made-Up Sound")]);
        assert!(!library.has_name(&["Made-Up Artist"], "Wave"));
        let library = untagged(&[("Made-Up Artist", "Wave")]);
        assert!(!library.has_name(&["Made-Up Artist"], "Wave Of Made-Up Sound"));
        // Five is enough.
        let library = untagged(&[("Made-Up Artist", "Waves Of Made-Up Sound")]);
        assert!(library.has_name(&["Made-Up Artist"], "Waves"));
    }

    #[test]
    fn an_artist_name_shorter_than_three_characters_doesn_t_match_inside_another() {
        let library = untagged(&[("Ab", "Made-Up Album")]);
        assert!(!library.has_name(&["Abc Made-Up"], "Made-Up Album Extra"));
        let library = untagged(&[("Abc", "Made-Up Album")]);
        assert!(library.has_name(&["Abc Made-Up"], "Made-Up Album Extra"));
    }

    #[test]
    fn an_artist_name_inside_another_only_by_letters_isn_t_inside_it() {
        // "fairportconvention" contains "air", but "Air" is not one of its words.
        let library = untagged(&[("Air", "Greatest Hits")]);
        assert!(!library.has_name(&["Fairport Convention"], "Greatest Hits"));
        let library = untagged(&[("Fairport Convention", "Greatest Hits")]);
        assert!(!library.has_name(&["Air"], "Greatest Hits"));
        // A run of whole words inside the other still is.
        let library = untagged(&[("Echospace", "Made-Up Album")]);
        assert!(library.has_name(&["Echospace and Michael Mantra"], "Made-Up Album"));
        let library = untagged(&[("Made-Up Artist", "Made-Up Album")]);
        assert!(library.has_name(&["Another Made-Up Artist Trio"], "Made-Up Album"));
        // Words that are all there but not next to each other aren't a run.
        assert!(!library.has_name(&["Made Another Up Artist"], "Made-Up Album"));
    }

    #[test]
    fn a_various_artists_album_doesn_t_exclude_by_its_artist() {
        let library = untagged(&[("Various Artists", "Made-Up Hits"), ("", "Made-Up Hits")]);
        // "variousartists" is inside the candidate's artist, and the titles are the same.
        assert!(!library.has_name(&["Various Artists Orchestra"], "Made-Up Hits"));
        assert!(!library.has_name(&["Various"], "Made-Up Hits"));
        assert!(!library.has_name(&["Made-Up Artist"], "Made-Up Hits"));
    }

    #[test]
    fn a_different_artist_with_the_same_title_isn_t_the_library_s() {
        let library = untagged(&[("Made-Up Artist", "Made-Up Album")]);
        assert!(!library.has_name(&["Someone Else"], "Made-Up Album"));
        assert!(!library.has_name(&["Someone Else"], "Made-Up Album [Made-Up Subtitle]"));
    }
}
