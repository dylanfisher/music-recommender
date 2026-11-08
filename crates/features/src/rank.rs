//! Choosing candidate folders from a search's responses (downloads.md, Candidates): group
//! the peers' files into folders, accept or reject each on its format, track count, titles
//! and durations only, then order the accepted by durations, the folder's name, quality and
//! the peer. Pure, over the internal [`Peer`] and [`File`], never slskd's JSON.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use catalog::download::{Candidate, CandidateFile, Format, MAX_CANDIDATES, Policy, Track};
use primitives::fold;

use crate::query::is_various;
use primitives::similarity::{self, LIKELY_TRACKLIST, Score};

/// The reason line when no peer has an audio file to judge.
pub(crate) const NO_AUDIO: &str = "no audio found";

/// A peer with no free upload slot and a queue longer than this is passed over.
const MAX_QUEUE: u64 = 500;

/// A folder may hold at most this many audio files more than the tracklist: bonus tracks,
/// hidden tracks.
const MAX_EXTRA_FILES: usize = 3;

/// With at least this many files whose length and track's are both known, the durations
/// gate the folder.
const DURATION_PAIRS: usize = 3;

/// Of those, at least this share must agree.
const DURATION_SHARE: f64 = 0.8;

/// `mp3_320`'s range for a variable bitrate's average, in kbps: a heuristic for V0 that
/// Approve's file read checks again.
const V0_KBPS: std::ops::RangeInclusive<u32> = 220..=270;

/// `mp3_320`'s constant bitrate.
const CBR_KBPS: u32 = 320;

/// A peer's answer, as ranking reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    /// The Soulseek user.
    pub username: String,
    /// Whether it has an upload slot free now.
    pub free_slot: bool,
    /// Its upload queue.
    pub queue_length: u64,
    /// Every file it answered with, locked ones included.
    pub files: Vec<File>,
}

/// A shared file, as ranking reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    /// The peer's full path, separated by `\` or `/`.
    pub path: String,
    /// Bytes.
    pub size: u64,
    /// Seconds.
    pub length: Option<u32>,
    /// kbps.
    pub bitrate: Option<u32>,
    /// Bits a sample.
    pub bit_depth: Option<u32>,
    /// Hz.
    pub sample_rate: Option<u32>,
    /// Whether the bitrate is variable.
    pub variable_bitrate: Option<bool>,
    /// The peer won't send it.
    pub locked: bool,
}

/// The release being searched for, from the job's snapshot.
#[derive(Clone, Copy, Debug)]
pub struct Release<'a> {
    /// As Discogs spells it; `Various` for a compilation.
    pub artist: &'a str,
    /// As Discogs spells it.
    pub title: &'a str,
    /// The release's year, if known.
    pub year: Option<i32>,
    /// One or two tracklists, the player's flattenings.
    pub tracklists: &'a [Vec<Track>],
}

/// The format an audio file's extension names; `None` for anything else, which isn't
/// downloaded.
fn audio_format(extension: &str) -> Option<Format> {
    let format = match extension.to_ascii_lowercase().as_str() {
        "m4a" => Format::Alac,
        other => Format::parse(other)?,
    };
    Some(format)
}

fn is_lossless(format: Format) -> bool {
    matches!(
        format,
        Format::Flac | Format::Alac | Format::Wav | Format::Ape | Format::Wv
    )
}

/// The format's name in a reason line.
fn label(format: Format) -> &'static str {
    match format {
        Format::Flac => "FLAC",
        Format::Alac => "ALAC",
        Format::Mp3 => "MP3",
        Format::Ogg => "Ogg",
        Format::Opus => "Opus",
        Format::Wav => "WAV",
        Format::Ape => "APE",
        Format::Wv => "WavPack",
    }
}

/// A peer's folder of audio files in one format, with disc folders merged under it.
struct Folder<'a> {
    peer: &'a Peer,
    /// The folder's path components.
    path: Vec<&'a str>,
    format: Format,
    /// The files of `format`, in natural order of `name`, with their names under the
    /// folder, `/`-separated.
    files: Vec<(String, &'a File)>,
}

/// How far a folder got before it was rejected; a later stage is a nearer miss.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Stage {
    Format,
    Count,
    Titles,
    Durations,
}

/// A rejected folder: how far it got, how close it came within that stage, and its line.
struct Rejection {
    stage: Stage,
    closeness: f64,
    format: Format,
    reason: String,
}

/// An accepted folder, with what orders it.
struct Accepted {
    candidate: Candidate,
    durations: f64,
    folder_match: i32,
    quality: (bool, u32),
    free_slot: bool,
    queue_length: u64,
    path: String,
}

/// The accepted candidates, best first, at most [`MAX_CANDIDATES`]; or, when none is, the
/// one line saying why the nearest miss missed ("FLAC, 9 of 12 tracks", "only MP3 found").
///
/// # Errors
///
/// The reason line when no folder is accepted.
pub fn rank(
    release: &Release<'_>,
    peers: &[Peer],
    policy: Policy,
) -> Result<Vec<Candidate>, String> {
    let folders: Vec<Folder<'_>> = peers
        .iter()
        .filter(|peer| peer.free_slot || peer.queue_length <= MAX_QUEUE)
        .flat_map(folders)
        .collect();
    if folders.is_empty() {
        return Err(NO_AUDIO.to_string());
    }
    let mut accepted = Vec::new();
    let mut rejected = Vec::new();
    for folder in &folders {
        match judge(release, folder, policy) {
            Ok(candidate) => accepted.push(candidate),
            Err(rejection) => rejected.push(rejection),
        }
    }
    if accepted.is_empty() {
        return Err(reason(&rejected));
    }
    accepted.sort_by(order);
    Ok(accepted
        .into_iter()
        .take(MAX_CANDIDATES)
        .map(|accepted| accepted.candidate)
        .collect())
}

/// The reason line of the nearest miss. Format misses name every format found.
fn reason(rejected: &[Rejection]) -> String {
    let nearest = rejected
        .iter()
        .max_by(|a, b| {
            a.stage
                .cmp(&b.stage)
                .then(a.closeness.total_cmp(&b.closeness))
        })
        .expect("a reason for at least one rejection");
    if nearest.stage != Stage::Format {
        return nearest.reason.clone();
    }
    let mut formats: Vec<Format> = rejected.iter().map(|r| r.format).collect();
    formats.sort_by_key(|f| Format::ALL.iter().position(|g| g == f));
    formats.dedup();
    if formats.len() == 1 {
        return nearest.reason.clone();
    }
    let names: Vec<&str> = formats.into_iter().map(label).collect();
    format!("only {} found", names.join(" or "))
}

/// Best first: the share of durations that agree, the folder's name, quality, a free slot,
/// a shorter queue, then the peer and the path so the order is always the same.
fn order(a: &Accepted, b: &Accepted) -> Ordering {
    b.durations
        .total_cmp(&a.durations)
        .then(b.folder_match.cmp(&a.folder_match))
        .then(b.quality.cmp(&a.quality))
        .then(b.free_slot.cmp(&a.free_slot))
        .then(a.queue_length.cmp(&b.queue_length))
        .then(a.candidate.peer.cmp(&b.candidate.peer))
        .then(a.path.cmp(&b.path))
}

/// A path's components, split on `\` and `/` once.
fn components(path: &str) -> Vec<&str> {
    path.split(['\\', '/']).filter(|c| !c.is_empty()).collect()
}

/// Whether a folder's name is a disc's: `CD1`, `Disc 2`, `Disc02`, `disk_3`.
fn is_disc_folder(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let rest = ["disc", "disk", "cd"]
        .iter()
        .find_map(|prefix| lower.strip_prefix(prefix));
    rest.is_some_and(|rest| {
        let digits = rest.trim_start_matches([' ', '_', '-', '.']);
        !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
    })
}

/// A peer's candidate folders: its unlocked audio files grouped by parent folder, disc
/// folders merged under theirs, each judged on its majority format.
fn folders(peer: &Peer) -> Vec<Folder<'_>> {
    let mut groups: BTreeMap<Vec<&str>, Vec<(String, &File, Format)>> = BTreeMap::new();
    for file in peer.files.iter().filter(|file| !file.locked) {
        let parts = components(&file.path);
        let Some((&name, parent)) = parts.split_last() else {
            continue;
        };
        let Some(format) = name.rsplit_once('.').and_then(|(_, ext)| audio_format(ext)) else {
            continue;
        };
        let (folder, name) = match parent.split_last() {
            Some((&disc, above)) if is_disc_folder(disc) => {
                (above.to_vec(), format!("{disc}/{name}"))
            }
            _ => (parent.to_vec(), name.to_string()),
        };
        groups.entry(folder).or_default().push((name, file, format));
    }
    groups
        .into_iter()
        .map(|(path, files)| {
            let mut counts: BTreeMap<usize, (usize, Format)> = BTreeMap::new();
            for (_, _, format) in &files {
                let rank = Format::ALL
                    .iter()
                    .position(|f| f == format)
                    .unwrap_or(usize::MAX);
                counts.entry(rank).or_insert((0, *format)).0 += 1;
            }
            // The most files; a tie goes to the format listed first.
            let format = counts
                .values()
                .fold(
                    None::<(usize, Format)>,
                    |best, &(count, format)| match best {
                        Some((most, _)) if most >= count => best,
                        _ => Some((count, format)),
                    },
                )
                .map(|(_, format)| format)
                .expect("a group has a file");
            let mut files: Vec<(String, &File)> = files
                .into_iter()
                .filter(|(_, _, f)| *f == format)
                .map(|(name, file, _)| (name, file))
                .collect();
            files.sort_by(|a, b| natural(&a.0, &b.0));
            Folder {
                peer,
                path,
                format,
                files,
            }
        })
        .collect()
}

/// Names in natural order: runs of digits compared as numbers, so `2` comes before `10`.
fn natural(a: &str, b: &str) -> Ordering {
    let chunks = |s: &str| -> Vec<(bool, String)> {
        let mut chunks: Vec<(bool, String)> = Vec::new();
        for c in s.chars() {
            let digit = c.is_ascii_digit();
            match chunks.last_mut() {
                Some((d, chunk)) if *d == digit => chunk.push(c),
                _ => chunks.push((digit, c.to_string())),
            }
        }
        chunks
    };
    let (a_chunks, b_chunks) = (chunks(a), chunks(b));
    for (x, y) in a_chunks.iter().zip(&b_chunks) {
        let ordering = match (x, y) {
            ((true, x), (true, y)) => {
                let (x, y) = (x.trim_start_matches('0'), y.trim_start_matches('0'));
                x.len().cmp(&y.len()).then_with(|| x.cmp(y))
            }
            ((_, x), (_, y)) => x.to_lowercase().cmp(&y.to_lowercase()),
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    a_chunks.len().cmp(&b_chunks.len()).then_with(|| a.cmp(b))
}

/// The title a file's name holds: its extension, a leading `01`, `1-03`, `A1` or `CD2-04`,
/// and an `Artist - ` prefix removed. Empty when the name is only a number.
fn file_title(name: &str, artist: &str) -> String {
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem).trim();
    let rest = without_track_number(stem);
    let rest = rest.trim_start_matches(|c: char| c.is_whitespace() || "-._)]".contains(c));
    match rest.split_once(" - ") {
        Some((left, right))
            if !artist.is_empty() && fold::key(left, false) == fold::key(artist, false) =>
        {
            right.trim().to_string()
        }
        _ => rest.trim().to_string(),
    }
}

/// `stem` without a leading track number: `CD2-04`, `1-03`, `01`, or a vinyl side's `A1`.
/// A letter and digits count only when a separator follows, so `B52s` keeps its name.
fn without_track_number(stem: &str) -> &str {
    let digits = |s: &str| s.len() - s.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let lower = stem.to_ascii_lowercase();
    let mut at = 0;
    if lower.starts_with("cd") && digits(&stem[2..]) > 0 {
        let after = 2 + digits(&stem[2..]);
        if stem[after..].starts_with(['-', '.', ' ']) {
            at = after + 1;
        }
    }
    let first = digits(&stem[at..]);
    if first > 0 {
        let mut end = at + first;
        if stem[end..].starts_with(['-', '.']) && digits(&stem[end + 1..]) > 0 {
            end += 1 + digits(&stem[end + 1..]);
        }
        return &stem[end..];
    }
    let mut chars = stem[at..].chars();
    if let Some(side) = chars.next()
        && side.is_ascii_alphabetic()
    {
        let after = at + 1;
        let count = digits(&stem[after..]);
        let end = after + count;
        if (1..=2).contains(&count) && stem[end..].starts_with([' ', '-', '.', '_']) {
            return &stem[end..];
        }
    }
    &stem[at..]
}

/// Whether the folder's files meet `policy`, or the reason line why not.
fn format_fits(folder: &Folder<'_>, policy: Policy) -> Result<(), String> {
    let format = folder.format;
    let fits = match policy {
        Policy::Flac => format == Format::Flac,
        Policy::Lossless => matches!(format, Format::Flac | Format::Alac),
        Policy::Any => true,
        Policy::Mp3320 => return mp3_320_fits(folder),
    };
    if fits {
        Ok(())
    } else {
        Err(format!("only {} found", label(format)))
    }
}

/// Whether an MP3 folder is CBR 320 or a V0 average, or the reason line why not.
fn mp3_320_fits(folder: &Folder<'_>) -> Result<(), String> {
    if folder.format != Format::Mp3 {
        return Err(format!("only {} found", label(folder.format)));
    }
    let mut files = folder.files.iter().map(|(_, f)| f);
    let constant = files
        .clone()
        .all(|f| f.bitrate == Some(CBR_KBPS) && f.variable_bitrate != Some(true));
    let variable = files.all(|f| f.variable_bitrate == Some(true));
    let average = average_bitrate(folder);
    if constant || (variable && average.is_some_and(|a| V0_KBPS.contains(&a))) {
        return Ok(());
    }
    Err(match average {
        Some(average) => format!("MP3 at {average} kbps"),
        None => "MP3 of unknown bitrate".to_string(),
    })
}

/// The folder's files' average bitrate in kbps, rounded; `None` if any file's is unknown.
fn average_bitrate(folder: &Folder<'_>) -> Option<u32> {
    let rates: Option<Vec<u32>> = folder.files.iter().map(|(_, f)| f.bitrate).collect();
    let rates = rates.filter(|rates| !rates.is_empty())?;
    let total: u64 = rates.iter().map(|&r| u64::from(r)).sum();
    let count = rates.len() as u64;
    u32::try_from((total + count / 2) / count).ok()
}

/// A share as a whole percent, for a reason line.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a share from 0 to 1"
)]
fn percent(share: f64) -> u32 {
    (share * 100.0).round() as u32
}

/// Accepts or rejects one folder on its format, count, titles and durations.
fn judge(
    release: &Release<'_>,
    folder: &Folder<'_>,
    policy: Policy,
) -> Result<Accepted, Rejection> {
    let reject = |stage, closeness, reason| Rejection {
        stage,
        closeness,
        format: folder.format,
        reason,
    };
    format_fits(folder, policy).map_err(|reason| reject(Stage::Format, 0.0, reason))?;

    let count = folder.files.len();
    let fitting = fitting_tracklists(release, folder)
        .map_err(|(closeness, reason)| reject(Stage::Count, closeness, reason))?;
    let titles: Vec<String> = folder
        .files
        .iter()
        .map(|(name, _)| {
            let base = name.rsplit_once('/').map_or(name.as_str(), |(_, n)| n);
            file_title(base, release.artist)
        })
        .collect();
    let numbers_only = titles
        .iter()
        .all(|t| fold::key(t, true).chars().all(|c| c.is_ascii_digit()));
    let files: Vec<similarity::File<'_>> = titles
        .iter()
        .zip(&folder.files)
        .map(|(title, (_, file))| similarity::File {
            title,
            seconds: file.length.map(f64::from),
        })
        .collect();
    let tracklists: Vec<Vec<similarity::Track<'_>>> = fitting
        .iter()
        .map(|&i| {
            release.tracklists[i]
                .iter()
                .map(|track| similarity::Track {
                    title: &track.title,
                    seconds: track.seconds,
                })
                .collect()
        })
        .collect();
    let (best, mut score) = Score::best(&files, &tracklists).expect("a fitting tracklist");
    let tracklist = fitting[best];
    if numbers_only {
        let tracks = release.tracklists[tracklist].len();
        let pairs = numbered_pairs(&folder.files, &release.tracklists[tracklist]);
        if !(folder_names_release(folder, release) && count == tracks && pairs >= DURATION_PAIRS) {
            return Err(reject(
                Stage::Titles,
                0.0,
                "file names are only numbers".to_string(),
            ));
        }
        // In order, as the player's same-count score maps them.
        score = Score::new(
            &folder
                .files
                .iter()
                .map(|(_, file)| similarity::File {
                    title: "",
                    seconds: file.length.map(f64::from),
                })
                .collect::<Vec<_>>(),
            &tracklists[best],
        );
    } else if score.tracklist < LIKELY_TRACKLIST {
        return Err(reject(
            Stage::Titles,
            score.tracklist,
            format!("titles match {}%", percent(score.tracklist)),
        ));
    }
    if let Some(share) = score.durations_share()
        && score.durations_compared >= DURATION_PAIRS
        && share < DURATION_SHARE
    {
        return Err(reject(
            Stage::Durations,
            share,
            format!("durations agree {}%", percent(share)),
        ));
    }
    Ok(candidate_of(release, folder, tracklist, &score))
}

/// The tracklists the folder's file count fits, at least as many files as tracks and at
/// most `MAX_EXTRA_FILES` more; or how near it came and the reason line.
fn fitting_tracklists(
    release: &Release<'_>,
    folder: &Folder<'_>,
) -> Result<Vec<usize>, (f64, String)> {
    let count = folder.files.len();
    let fitting: Vec<usize> = (0..release.tracklists.len())
        .filter(|&i| {
            let tracks = release.tracklists[i].len();
            count >= tracks && count <= tracks + MAX_EXTRA_FILES
        })
        .collect();
    if !fitting.is_empty() {
        return Ok(fitting);
    }
    let nearest = release
        .tracklists
        .iter()
        .map(Vec::len)
        .min_by_key(|&t| t.abs_diff(count))
        .unwrap_or(0);
    let reason = if count < nearest {
        format!("{}, {count} of {nearest} tracks", label(folder.format))
    } else {
        format!(
            "{}, {count} files for {nearest} tracks",
            label(folder.format)
        )
    };
    #[expect(clippy::cast_precision_loss, reason = "track counts")]
    let closeness = -(nearest.abs_diff(count) as f64);
    Err((closeness, reason))
}

/// Files and tracks, paired in order, whose lengths are both known.
fn numbered_pairs(files: &[(String, &File)], tracks: &[Track]) -> usize {
    files
        .iter()
        .zip(tracks)
        .filter(|((_, file), track)| file.length.is_some() && track.seconds.is_some())
        .count()
}

/// The folder's last two path components, folded to a key.
fn folder_key(folder: &Folder<'_>) -> String {
    let start = folder.path.len().saturating_sub(2);
    fold::key(&folder.path[start..].join(" "), false)
}

/// Whether the folder's name holds the release's artist and title.
fn folder_names_release(folder: &Folder<'_>, release: &Release<'_>) -> bool {
    let key = folder_key(folder);
    let title = fold::key(release.title, true);
    let artist = fold::key(release.artist, false);
    !title.is_empty()
        && key.contains(&title)
        && (is_various(release.artist) || key.contains(&artist))
}

/// How well the folder's name names the release: one for the title, one for the artist,
/// less one when it names years and none is within 1 of the release's.
fn folder_match(folder: &Folder<'_>, release: &Release<'_>) -> i32 {
    let key = folder_key(folder);
    let title = fold::key(release.title, true);
    let artist = fold::key(release.artist, false);
    let mut value = i32::from(!title.is_empty() && key.contains(&title))
        + i32::from(!is_various(release.artist) && !artist.is_empty() && key.contains(&artist));
    if let Some(year) = release.year {
        let start = folder.path.len().saturating_sub(2);
        let years = years(&folder.path[start..].join(" "));
        if !years.is_empty() && years.iter().all(|y| (y - year).abs() > 1) {
            value -= 1;
        }
    }
    value
}

/// The four-digit years from 1900 to 2099 standing alone in `text`.
fn years(text: &str) -> Vec<i32> {
    text.split(|c: char| !c.is_ascii_digit())
        .filter(|run| run.len() == 4)
        .filter_map(|run| run.parse().ok())
        .filter(|year| (1900..=2099).contains(year))
        .collect()
}

/// The accepted folder as a candidate, its files in release order, extras last.
fn candidate_of(
    release: &Release<'_>,
    folder: &Folder<'_>,
    tracklist: usize,
    score: &Score,
) -> Accepted {
    let tracks = &release.tracklists[tracklist];
    let mut files: Vec<(Option<usize>, CandidateFile)> = folder
        .files
        .iter()
        .zip(&score.mapping)
        .map(|((name, file), track)| {
            (
                *track,
                CandidateFile {
                    remote: file.path.clone(),
                    name: name.clone(),
                    size: file.size,
                    position: track.map(|t| tracks[t].position.clone()),
                },
            )
        })
        .collect();
    // Stable: extras keep their folder order after the matched files.
    files.sort_by_key(|(track, _)| track.unwrap_or(usize::MAX));
    let first = folder.files[0].1;
    let lossless = is_lossless(folder.format);
    let bitrate = average_bitrate(folder);
    Accepted {
        candidate: Candidate {
            peer: folder.peer.username.clone(),
            format: folder.format,
            bit_depth: first.bit_depth,
            sample_rate: first.sample_rate,
            bitrate: if lossless { None } else { bitrate },
            variable_bitrate: if lossless {
                None
            } else {
                first.variable_bitrate
            },
            tracklist,
            files: files.into_iter().map(|(_, file)| file).collect(),
        },
        durations: score.durations_share().unwrap_or(0.5),
        folder_match: folder_match(folder, release),
        // Lossless before lossy; bit depth and sample rate never count, so 16-bit isn't
        // passed over for 24-bit. Lossy by bitrate.
        quality: (lossless, if lossless { 0 } else { bitrate.unwrap_or(0) }),
        free_slot: folder.peer.free_slot,
        queue_length: folder.peer.queue_length,
        path: folder.path.join("/"),
    }
}

#[cfg(test)]
mod tests {
    use catalog::download::{Policy, Track};

    use super::{File, Peer, Release, file_title, is_disc_folder, natural, rank};

    fn tracks(titles: &[&str]) -> Vec<Track> {
        titles
            .iter()
            .enumerate()
            .map(|(i, title)| Track {
                position: (i + 1).to_string(),
                title: (*title).to_string(),
                seconds: Some(200 + 10 * u32::try_from(i).unwrap()),
            })
            .collect()
    }

    fn flac(path: &str, length: Option<u32>) -> File {
        File {
            path: path.to_string(),
            size: 1000,
            length,
            bitrate: None,
            bit_depth: Some(16),
            sample_rate: Some(44100),
            variable_bitrate: None,
            locked: false,
        }
    }

    /// A peer with a folder of FLAC files named for `titles`, each `seconds[i]` long.
    fn peer(name: &str, folder: &str, titles: &[&str], seconds: &[Option<u32>]) -> Peer {
        Peer {
            username: name.to_string(),
            free_slot: true,
            queue_length: 0,
            files: titles
                .iter()
                .zip(seconds)
                .enumerate()
                .map(|(i, (title, s))| flac(&format!("{folder}\\{:02} {title}.flac", i + 1), *s))
                .collect(),
        }
    }

    const TITLES: [&str; 4] = ["Alpha", "Bravo", "Charlie", "Delta"];

    fn release(tracklists: &[Vec<Track>]) -> Release<'_> {
        Release {
            artist: "Made-Up Artist",
            title: "Made-Up Album",
            year: Some(1999),
            tracklists,
        }
    }

    fn lengths() -> Vec<Option<u32>> {
        vec![Some(200), Some(210), Some(220), Some(230)]
    }

    #[test]
    fn three_extra_files_are_accepted_and_four_are_not() {
        // Eight tracks, so eight paired of eleven files still scores above Likely.
        let eight = ["A", "B", "C", "D", "E", "F", "G", "H"].map(|t| format!("Song {t}"));
        let eight: Vec<&str> = eight.iter().map(String::as_str).collect();
        let lists = [tracks(&eight)];
        let mut titles = eight.clone();
        titles.extend(["Extra One", "Extra Two", "Extra Three"]);
        let seconds = vec![None; titles.len()];
        let three = peer("p", "A\\Album", &titles, &seconds);
        let found = rank(&release(&lists), &[three], Policy::Flac).unwrap();
        assert_eq!(found[0].files.len(), 11);
        assert_eq!(found[0].files[10].position, None);
        assert_eq!(found[0].files[7].position.as_deref(), Some("8"));
        titles.push("Extra Four");
        let seconds = vec![None; titles.len()];
        let four = peer("p", "A\\Album", &titles, &seconds);
        assert_eq!(
            rank(&release(&lists), &[four], Policy::Flac).unwrap_err(),
            "FLAC, 12 files for 8 tracks"
        );
    }

    #[test]
    fn the_duration_gate_needs_three_pairs_and_takes_exactly_80_percent() {
        let five = ["A", "B", "C", "D", "E"].map(|t| format!("Song {t}"));
        let five: Vec<&str> = five.iter().map(String::as_str).collect();
        let lists = [tracks(&five)];
        // Four of five agree: exactly 80%.
        let seconds = [Some(200), Some(210), Some(220), Some(230), Some(999)];
        let edge = peer("p", "A\\Album", &five, &seconds);
        assert!(rank(&release(&lists), &[edge], Policy::Flac).is_ok());
        let seconds = [Some(200), Some(210), Some(220), Some(999), Some(999)];
        let under = peer("p", "A\\Album", &five, &seconds);
        assert_eq!(
            rank(&release(&lists), &[under], Policy::Flac).unwrap_err(),
            "durations agree 60%"
        );
        // Two known pairs, both wrong: too few to gate.
        let seconds = [Some(999), Some(999), None, None, None];
        let few = peer("p", "A\\Album", &five, &seconds);
        assert!(rank(&release(&lists), &[few], Policy::Flac).is_ok());
    }

    #[test]
    fn titles_at_the_likely_threshold_pass_and_below_fail() {
        let lists = [tracks(&TITLES)];
        let off = peer(
            "p",
            "A\\Album",
            &["Alpha", "Bravo", "Zzzzzzz", "Qqqqq"],
            &[None; 4],
        );
        assert_eq!(
            rank(&release(&lists), &[off], Policy::Flac).unwrap_err(),
            "titles match 50%"
        );
    }

    #[test]
    fn durations_order_before_the_folder_name_quality_and_the_peer() {
        let five = ["A", "B", "C", "D", "E"].map(|t| format!("Song {t}"));
        let five: Vec<&str> = five.iter().map(String::as_str).collect();
        let lists = [tracks(&five)];
        // Four of five agree: accepted, and behind the folder whose five all do.
        let seconds = [Some(200), Some(210), Some(220), Some(230), Some(999)];
        let named_but_off = peer(
            "named",
            "Made-Up Artist\\Made-Up Album (1999)",
            &five,
            &seconds,
        );
        let seconds = [Some(200), Some(210), Some(220), Some(230), Some(240)];
        let mut unnamed = peer("unnamed", "music\\stuff", &five, &seconds);
        unnamed.free_slot = false;
        unnamed.queue_length = 400;
        let found = rank(&release(&lists), &[named_but_off, unnamed], Policy::Flac).unwrap();
        let peers: Vec<&str> = found.iter().map(|c| c.peer.as_str()).collect();
        assert_eq!(peers, ["unnamed", "named"]);
    }

    #[test]
    fn the_folder_name_orders_before_the_peer_and_a_wrong_year_counts_against_it() {
        let lists = [tracks(&TITLES)];
        let right = Peer {
            free_slot: false,
            queue_length: 400,
            ..peer(
                "right",
                "Made-Up Artist\\Made-Up Album (1999)",
                &TITLES,
                &lengths(),
            )
        };
        let wrong_year = peer(
            "wrong-year",
            "Made-Up Artist\\Made-Up Album (2004)",
            &TITLES,
            &lengths(),
        );
        let found = rank(&release(&lists), &[wrong_year, right], Policy::Flac).unwrap();
        let peers: Vec<&str> = found.iter().map(|c| c.peer.as_str()).collect();
        assert_eq!(peers, ["right", "wrong-year"]);
    }

    #[test]
    fn a_peer_with_no_slot_and_a_long_queue_is_passed_over() {
        let lists = [tracks(&TITLES)];
        let busy = Peer {
            free_slot: false,
            queue_length: 501,
            ..peer("busy", "A\\Album", &TITLES, &lengths())
        };
        assert_eq!(
            rank(&release(&lists), &[busy], Policy::Flac).unwrap_err(),
            "no audio found"
        );
    }

    #[test]
    fn a_file_title_loses_its_number_and_artist() {
        assert_eq!(file_title("01 - First Song.flac", "X"), "First Song");
        assert_eq!(file_title("1-03 Third.flac", "X"), "Third");
        assert_eq!(file_title("CD2-04 Fourth.flac", "X"), "Fourth");
        assert_eq!(file_title("A1 Side Opener.mp3", "X"), "Side Opener");
        assert_eq!(file_title("B52s Song.mp3", "X"), "B52s Song");
        assert_eq!(
            file_title("03. Made-Up Artist - Third Song.flac", "Made-Up Artist"),
            "Third Song"
        );
        assert_eq!(file_title("07.flac", "X"), "");
        assert_eq!(file_title("Track_05.flac", "X"), "Track_05");
    }

    #[test]
    fn disc_folders_and_natural_order() {
        for name in ["CD1", "cd 2", "Disc02", "Disk_3", "DISC-4"] {
            assert!(is_disc_folder(name), "{name}");
        }
        for name in ["CD", "Discography", "CD1 Bonus", "Album"] {
            assert!(!is_disc_folder(name), "{name}");
        }
        let mut names = vec!["10 x", "9 x", "CD2/1 a", "CD1/10 b", "CD1/2 c"];
        names.sort_by(|a, b| natural(a, b));
        assert_eq!(names, ["9 x", "10 x", "CD1/2 c", "CD1/10 b", "CD2/1 a"]);
    }
}
