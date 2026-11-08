//! How alike titles are, and how well a list of files fits a tracklist: the player's
//! `TitleSimilarity`, `MatchThreshold` and `ReleaseScore` (its ADR 0131), ported for the
//! downloads' second copy of the matching rules (ADR 0010). `rank-golden.json` holds cases
//! both sides check.

use crate::fold;

/// How alike two folded titles must be, from 0 to 1, to count as the same track's.
pub const SAME_TITLE: f64 = 0.85;
/// The similarity a title contained in the other counts as, when the shorter has at least
/// `CONTAINED_LETTERS`: "Track" against "Track Original Mix".
pub const CONTAINED_TITLE: f64 = 0.9;
/// See `CONTAINED_TITLE`.
pub const CONTAINED_LETTERS: usize = 3;
/// The least similarity at which the alignment pairs a file with a track, when the counts
/// differ.
pub const PAIRED_TITLE: f64 = 0.5;
/// The tracklist score, from 0 to 1, a folder needs.
pub const LIKELY_TRACKLIST: f64 = 0.6;
/// A file's length agrees with Discogs's within this many seconds, or `DURATION_FRACTION`
/// of Discogs's, whichever is more.
pub const DURATION_SECONDS: f64 = 8.0;
/// See `DURATION_SECONDS`.
pub const DURATION_FRACTION: f64 = 0.05;
/// How much the share of agreeing durations counts beside the titles in `Score::rank`. A
/// tracklist with no known durations counts half.
pub const DURATION_WEIGHT: f64 = 0.25;

/// The similarity of two titles, each folded as a title ([`fold::key`]).
#[must_use]
pub fn title_similarity(a: &str, b: &str) -> f64 {
    let a: Vec<char> = fold::key(a, true).chars().collect();
    let b: Vec<char> = fold::key(b, true).chars().collect();
    similarity(&a, &b)
}

/// 1 for the same folded title, else one less the edit distance over the longer length,
/// raised to `CONTAINED_TITLE` when one holds the other and the shorter has at least
/// `CONTAINED_LETTERS`. An empty title is like nothing.
#[must_use]
pub fn similarity(a: &[char], b: &[char]) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    if a == b {
        return 1.0;
    }
    let (shorter, longer) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let ratio = 1.0 - count(edit_distance(a, b)) / count(longer.len());
    if shorter.len() >= CONTAINED_LETTERS && longer.windows(shorter.len()).any(|w| w == shorter) {
        ratio.max(CONTAINED_TITLE)
    } else {
        ratio
    }
}

/// Whether a file of `seconds` is a Discogs track of `expected` seconds.
#[must_use]
pub fn agrees(seconds: f64, expected: u32) -> bool {
    let expected = f64::from(expected);
    (seconds - expected).abs() <= DURATION_SECONDS.max(DURATION_FRACTION * expected)
}

/// A count as a float. The counts here are titles' lengths and tracklists', far below
/// 2^52.
#[expect(
    clippy::cast_precision_loss,
    reason = "lengths and counts far below 2^52"
)]
fn count(n: usize) -> f64 {
    n as f64
}

/// Levenshtein's distance, a row at a time.
fn edit_distance(a: &[char], b: &[char]) -> usize {
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = previous.clone();
    for i in 1..=a.len() {
        current[0] = i;
        for j in 1..=b.len() {
            current[j] = (previous[j] + 1)
                .min(current[j - 1] + 1)
                .min(previous[j - 1] + usize::from(a[i - 1] != b[j - 1]));
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

/// A track of a tracklist, as the score reads it.
#[derive(Clone, Debug, PartialEq)]
pub struct Track<'a> {
    /// As Discogs spells it.
    pub title: &'a str,
    /// Discogs's length, if it has one.
    pub seconds: Option<u32>,
}

/// A file, as the score reads it.
#[derive(Clone, Debug, PartialEq)]
pub struct File<'a> {
    /// The file's title, already cut from its name.
    pub title: &'a str,
    /// Its length, if the peer reported one.
    pub seconds: Option<f64>,
}

/// How well files fit a tracklist: the player's `ReleaseScore`.
#[derive(Clone, Debug, PartialEq)]
pub struct Score {
    /// For each file in order, the index of the track it is, or `None`.
    pub mapping: Vec<Option<usize>>,
    /// The same number of files as tracks.
    pub same_count: bool,
    /// The paired titles' similarity over the longer of the two lists, from 0 to 1.
    pub tracklist: f64,
    /// Same count and every file's title the same as its track's, in order.
    pub titles_in_order: bool,
    /// Paired files whose length and whose track's are both known.
    pub durations_compared: usize,
    /// Of those, the ones that agree.
    pub durations_agreeing: usize,
}

impl Score {
    /// Scores `files` against `tracks`. Same count: file i is track i. Different counts:
    /// the in-order alignment with the most similarity, pairing only titles at least
    /// `PAIRED_TITLE` alike, so a bonus or a missing track leaves the rest paired.
    #[must_use]
    pub fn new(files: &[File<'_>], tracks: &[Track<'_>]) -> Self {
        let file_keys: Vec<Vec<char>> = files
            .iter()
            .map(|file| fold::key(file.title, true).chars().collect())
            .collect();
        // rows[track][file]: their similarity.
        let rows: Vec<Vec<f64>> = tracks
            .iter()
            .map(|track| {
                let key: Vec<char> = fold::key(track.title, true).chars().collect();
                file_keys
                    .iter()
                    .map(|file| similarity(file, &key))
                    .collect()
            })
            .collect();
        let same_count = tracks.len() == files.len();
        let mut titles_in_order = same_count && !tracks.is_empty();
        let (mapping, total) = if same_count {
            let mut total = 0.0;
            for (index, row) in rows.iter().enumerate() {
                total += row[index];
                if row[index] < SAME_TITLE {
                    titles_in_order = false;
                }
            }
            ((0..tracks.len()).map(Some).collect(), total)
        } else {
            aligned(&rows, files.len())
        };
        let mut durations_compared = 0;
        let mut durations_agreeing = 0;
        for (file, track) in mapping.iter().enumerate() {
            let (Some(track), Some(seconds)) = (track, files[file].seconds) else {
                continue;
            };
            let Some(expected) = tracks[*track].seconds else {
                continue;
            };
            durations_compared += 1;
            if agrees(seconds, expected) {
                durations_agreeing += 1;
            }
        }
        let longer = tracks.len().max(files.len());
        Self {
            mapping,
            same_count,
            tracklist: if longer == 0 {
                0.0
            } else {
                total / count(longer)
            },
            titles_in_order,
            durations_compared,
            durations_agreeing,
        }
    }

    /// The share of compared durations that agree; `None` when none were compared.
    #[must_use]
    pub fn durations_share(&self) -> Option<f64> {
        (self.durations_compared > 0)
            .then(|| count(self.durations_agreeing) / count(self.durations_compared))
    }

    /// The tracklist, plus `DURATION_WEIGHT` times the share of agreeing durations, half
    /// when none were compared.
    #[must_use]
    pub fn rank(&self) -> f64 {
        self.tracklist + DURATION_WEIGHT * self.durations_share().unwrap_or(0.5)
    }

    /// What two scores of one folder against a release's flattenings are ordered by, best
    /// last: titles in order, then likely, then `rank`, as the player's `order`.
    #[must_use]
    pub fn order(&self) -> (bool, bool, f64) {
        (
            self.same_count && self.titles_in_order,
            self.same_count && self.tracklist >= LIKELY_TRACKLIST,
            self.rank(),
        )
    }

    /// The best of the scores of `files` against each of `tracklists`, with its index: the
    /// first of the best when they tie, as the player keeps the first. `None` for no
    /// tracklists.
    #[must_use]
    pub fn best(files: &[File<'_>], tracklists: &[Vec<Track<'_>>]) -> Option<(usize, Self)> {
        let mut best: Option<(usize, Self)> = None;
        for (index, tracks) in tracklists.iter().enumerate() {
            let score = Self::new(files, tracks);
            if best
                .as_ref()
                .is_none_or(|(_, kept)| greater(score.order(), kept.order()))
            {
                best = Some((index, score));
            }
        }
        best
    }
}

/// `a > b` for orders, compared as the player's tuples are.
fn greater(a: (bool, bool, f64), b: (bool, bool, f64)) -> bool {
    (a.0, a.1)
        .cmp(&(b.0, b.1))
        .then_with(|| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal))
        == std::cmp::Ordering::Greater
}

/// The in-order pairing of files with tracks that has the most similarity, and that
/// similarity. `rows[track][file]`.
fn aligned(rows: &[Vec<f64>], files: usize) -> (Vec<Option<usize>>, f64) {
    let tracks = rows.len();
    // best[t][f]: the most similarity pairing the first t tracks with the first f files.
    let mut best = vec![vec![0.0_f64; files + 1]; tracks + 1];
    for t in 1..=tracks {
        for f in 1..=files {
            let mut value = best[t - 1][f].max(best[t][f - 1]);
            let similarity = rows[t - 1][f - 1];
            if similarity >= PAIRED_TITLE {
                value = value.max(best[t - 1][f - 1] + similarity);
            }
            best[t][f] = value;
        }
    }
    let mut mapping = vec![None; files];
    let (mut t, mut f) = (tracks, files);
    #[expect(
        clippy::float_cmp,
        reason = "retracing the table: the values compared were computed the same way"
    )]
    while t > 0 && f > 0 {
        let similarity = rows[t - 1][f - 1];
        if similarity >= PAIRED_TITLE && best[t][f] == best[t - 1][f - 1] + similarity {
            mapping[f - 1] = Some(t - 1);
            t -= 1;
            f -= 1;
        } else if best[t][f] == best[t - 1][f] {
            t -= 1;
        } else {
            f -= 1;
        }
    }
    (mapping, best[tracks][files])
}

#[cfg(test)]
mod tests {
    use super::{File, Score, Track, agrees, title_similarity};

    #[test]
    fn titles_compare_by_their_folded_keys() {
        assert!((title_similarity("First Song", "first song!") - 1.0).abs() < 1e-12);
        assert!((title_similarity("Track", "Track (Original Mix)") - 0.9).abs() < 1e-12);
        // "ab" is too short to count as contained.
        assert!((title_similarity("ab", "abcd") - 0.5).abs() < 1e-12);
        assert!(title_similarity("", "x").abs() < 1e-12);
        assert!((title_similarity("kitten", "sitting") - (1.0 - 3.0 / 7.0)).abs() < 1e-12);
    }

    #[test]
    fn durations_agree_within_8_seconds_or_5_percent() {
        assert!(agrees(108.0, 100));
        assert!(!agrees(108.5, 100));
        // 5% of 400 is 20 s.
        assert!(agrees(420.0, 400));
        assert!(!agrees(420.5, 400));
    }

    #[test]
    fn a_bonus_track_leaves_the_rest_paired() {
        let tracks = [
            Track {
                title: "One",
                seconds: Some(100),
            },
            Track {
                title: "Two",
                seconds: Some(200),
            },
        ];
        let files = [
            File {
                title: "One",
                seconds: Some(101.0),
            },
            File {
                title: "Bonus Mix",
                seconds: None,
            },
            File {
                title: "Two",
                seconds: Some(300.0),
            },
        ];
        let score = Score::new(&files, &tracks);
        assert_eq!(score.mapping, [Some(0), None, Some(1)]);
        assert!(!score.same_count);
        assert!((score.tracklist - 2.0 / 3.0).abs() < 1e-12);
        assert_eq!((score.durations_compared, score.durations_agreeing), (2, 1));
    }

    #[test]
    fn the_best_flattening_wins_and_the_first_of_a_tie() {
        let parts = vec![
            Track {
                title: "Suite I",
                seconds: None,
            },
            Track {
                title: "Suite II",
                seconds: None,
            },
        ];
        let whole = vec![Track {
            title: "Suite",
            seconds: None,
        }];
        let files = [File {
            title: "Suite",
            seconds: None,
        }];
        let (index, score) = Score::best(&files, &[parts.clone(), whole]).unwrap();
        assert_eq!(index, 1);
        assert!(score.titles_in_order);
        let (index, _) = Score::best(&files, &[parts.clone(), parts]).unwrap();
        assert_eq!(index, 0);
    }
}
