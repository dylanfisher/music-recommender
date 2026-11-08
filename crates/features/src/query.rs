//! The Soulseek searches for a release (downloads.md, Queries): two per attempt, stopping at
//! the first with an accepted candidate, each of folded words with no format word and no
//! `-word` exclusions.

use std::time::Duration;

use primitives::fold;

/// How long slskd collects responses to one search. The client sends it in milliseconds.
pub const SEARCH_TIMEOUT: Duration = Duration::from_secs(15);

/// The folded keys slskd's "Various" and "Various Artists" fold to.
const VARIOUS: [&str; 2] = ["various", "variousartists"];

/// The search texts for one attempt, in order: (1) the artist's words and the title's;
/// (2) the title's words and the artist's longest word. For Various Artists, the title's
/// words alone. A second text with the same words as the first is left out, since slskd
/// matches words in any order.
#[must_use]
pub fn queries(artist: &str, title: &str) -> Vec<String> {
    let title = fold::words(title, true);
    let artist = fold::words(artist, false);
    if artist.is_empty() || VARIOUS.contains(&artist.concat().as_str()) {
        return vec![title.join(" ")];
    }
    let first = [artist.as_slice(), title.as_slice()].concat();
    let mut queries = vec![first.join(" ")];
    // The first of the longest, so the query doesn't depend on a sort.
    let longest = artist
        .iter()
        .fold(&artist[0], |kept, word| {
            if word.chars().count() > kept.chars().count() {
                word
            } else {
                kept
            }
        })
        .clone();
    let mut second = title.clone();
    second.push(longest);
    let mut first_sorted = first;
    first_sorted.sort();
    let mut second_sorted = second.clone();
    second_sorted.sort();
    if second_sorted != first_sorted && !title.is_empty() {
        queries.push(second.join(" "));
    }
    queries
}

/// Whether `artist` is Discogs's compilation artist.
pub(crate) fn is_various(artist: &str) -> bool {
    VARIOUS.contains(&fold::key(artist, false).as_str())
}

#[cfg(test)]
mod tests {
    use super::{SEARCH_TIMEOUT, queries};

    #[test]
    fn the_search_timeout_is_15_000_milliseconds() {
        assert_eq!(SEARCH_TIMEOUT.as_millis(), 15_000);
    }

    #[test]
    fn two_queries_the_second_with_the_artist_s_longest_word() {
        assert_eq!(
            queries("The Made-Up Artist (2)", "Made-Up Album (Deluxe Edition)"),
            ["made up artist made up album", "made up album artist"]
        );
        assert_eq!(
            queries("Sigur Rós", "Ágætis byrjun"),
            ["sigur ros agaetis byrjun", "agaetis byrjun sigur"]
        );
    }

    #[test]
    fn various_artists_search_by_the_title_alone() {
        assert_eq!(queries("Various", "Made-Up Hits"), ["made up hits"]);
        assert_eq!(queries("Various Artists", "Made-Up Hits"), ["made up hits"]);
    }

    #[test]
    fn a_one_word_artist_needs_one_query() {
        assert_eq!(queries("Björk", "Post"), ["bjork post"]);
    }
}
