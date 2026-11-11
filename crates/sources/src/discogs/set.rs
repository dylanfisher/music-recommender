//! A month's set of Discogs dumps in the NAS folder, under Discogs's own file names:
//! `discogs_YYYYMMDD_{artists,labels,masters,releases}.xml.gz` and
//! `discogs_YYYYMMDD_CHECKSUM.txt`, which lists each dump's SHA-256 and name, one a line. A
//! file being downloaded is `.discogs_YYYYMMDD_<dump>.part`, hidden from the player's
//! tagger, which looks for Discogs's names. A set is complete when its CHECKSUM has its real
//! name, lists the four dumps, and the four exist; the download renames the
//! CHECKSUM last, so a set is never complete before its dumps are.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// One of a set's four dumps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dump {
    /// `artists`: names, aliases, groups and members.
    Artists,
    /// `labels`: names and parents.
    Labels,
    /// `masters`: what groups a release's versions, and its main release.
    Masters,
    /// `releases`: the releases themselves.
    Releases,
}

impl Dump {
    /// The four, in the order an import reads them.
    pub const ALL: [Self; 4] = [Self::Artists, Self::Labels, Self::Masters, Self::Releases];

    /// The dump's name in its file name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Artists => "artists",
            Self::Labels => "labels",
            Self::Masters => "masters",
            Self::Releases => "releases",
        }
    }
}

/// The checksum file's part of its name, where a dump has its own name.
const CHECKSUM: &str = "CHECKSUM";
const PREFIX: &str = "discogs_";
const DUMP_SUFFIX: &str = ".xml.gz";
const CHECKSUM_SUFFIX: &str = ".txt";
const PART_SUFFIX: &str = ".part";

/// `dump`'s file name in the set of `date`, `YYYYMMDD`.
#[must_use]
pub fn file_name(date: &str, dump: Dump) -> String {
    format!("{PREFIX}{date}_{}{DUMP_SUFFIX}", dump.name())
}

/// The CHECKSUM file's name in the set of `date`.
#[must_use]
pub fn checksum_name(date: &str) -> String {
    format!("{PREFIX}{date}_{CHECKSUM}{CHECKSUM_SUFFIX}")
}

/// The date of a set's CHECKSUM file `name`, under its real name; `None` for any other.
#[must_use]
pub fn checksum_date(name: &str) -> Option<&str> {
    match parse(name) {
        Some((date, CHECKSUM)) if !name.starts_with('.') => Some(date),
        _ => None,
    }
}

/// The temporary name a download of the file `name` is written under, `None` when `name`
/// isn't one of a set's files.
#[must_use]
pub fn part_name(name: &str) -> Option<String> {
    if name.starts_with('.') {
        return None;
    }
    let (date, what) = parse(name)?;
    Some(format!(".{PREFIX}{date}_{what}{PART_SUFFIX}"))
}

/// The date and the dump name (or `CHECKSUM`) of one of a set's files or parts; `None` for
/// any other name.
pub(crate) fn parse(name: &str) -> Option<(&str, &str)> {
    let (rest, dump_suffix, checksum_suffix) = match name.strip_prefix('.') {
        Some(hidden) => (hidden, PART_SUFFIX, PART_SUFFIX),
        None => (name, DUMP_SUFFIX, CHECKSUM_SUFFIX),
    };
    let (date, what) = rest.strip_prefix(PREFIX)?.split_once('_')?;
    if date.len() != 8 || !date.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if let Some(dump) = Dump::ALL
        .iter()
        .map(|dump| dump.name())
        .find(|&dump| what.strip_suffix(dump_suffix) == Some(dump))
    {
        return Some((date, dump));
    }
    (what.strip_suffix(checksum_suffix) == Some(CHECKSUM)).then_some((date, CHECKSUM))
}

/// The (file name, SHA-256) pairs of a CHECKSUM file's text, in its order. Each line is 64
/// lowercase hex digits, a space, then the name, after a second space or a `*` in
/// `sha256sum`'s own format; blank lines are skipped.
///
/// # Errors
///
/// A line that isn't that shape, named by its number.
pub fn checksums(text: &str) -> Result<Vec<(String, String)>, String> {
    let mut listed = Vec::new();
    for (number, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let bad = || format!("line {} isn't a SHA-256 and a file name", number + 1);
        let (hash, rest) = line.split_at_checked(64).ok_or_else(bad)?;
        // `sha256sum` writes two spaces, or a space and `*`; Discogs writes one space.
        let name = rest
            .strip_prefix(' ')
            .map(|name| name.strip_prefix([' ', '*']).unwrap_or(name))
            .filter(|name| !name.is_empty())
            .ok_or_else(bad)?;
        if !hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(bad());
        }
        listed.push((name.to_string(), hash.to_string()));
    }
    Ok(listed)
}

/// Whether the set of `date` is complete among the file names `present`, its CHECKSUM's
/// text being `checksum`: the CHECKSUM lists the four dumps and they are all present. Any
/// other line it has (Discogs has only ever listed the four) is ignored, here and in the
/// fetch, which downloads only the four; otherwise a set would never be complete and would
/// be fetched again every day.
///
/// # Errors
///
/// When the CHECKSUM can't be read.
fn is_complete(present: &[String], date: &str, checksum: &str) -> Result<bool, String> {
    let listed = checksums(checksum)?;
    Ok(Dump::ALL.iter().all(|&dump| {
        let name = file_name(date, dump);
        listed.iter().any(|(listed, _)| *listed == name) && present.contains(&name)
    }))
}

/// The files among `present` that pruning to the set of `date` may delete: every file,
/// parts included, of a set older than `date`. Never a file that isn't a set's, nor one of
/// `date`'s set or a newer one, which may be downloading or not imported yet.
fn prunable(present: &[String], date: &str) -> Vec<String> {
    present
        .iter()
        .filter(|name| parse(name).is_some_and(|(set, _)| set < date))
        .cloned()
        .collect()
}

/// A complete set: its folder, its date, and the SHA-256 its CHECKSUM lists for each file.
#[derive(Debug)]
pub struct Set {
    /// The folder the files are in.
    pub dir: PathBuf,
    /// The set's date as its file names have it, `YYYYMMDD`.
    pub date: String,
    checksums: Vec<(String, String)>,
}

impl Set {
    /// Where `dump` is.
    #[must_use]
    pub fn path(&self, dump: Dump) -> PathBuf {
        self.dir.join(file_name(&self.date, dump))
    }

    /// The SHA-256 the CHECKSUM lists for `dump`, as lowercase hex. A complete set lists
    /// every dump, so this is never empty.
    #[must_use]
    pub fn sha256(&self, dump: Dump) -> &str {
        let name = file_name(&self.date, dump);
        self.checksums
            .iter()
            .find(|(listed, _)| *listed == name)
            .map_or("", |(_, hash)| hash)
    }
}

/// Why the folder couldn't be read.
#[derive(Debug)]
pub enum SetError {
    /// The folder or a CHECKSUM file can't be read. Holds its path.
    Io(PathBuf, io::Error),
    /// A CHECKSUM file isn't in `sha256sum` format. Holds its path and the line at fault.
    Checksum(PathBuf, String),
}

impl fmt::Display for SetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(path, error) => write!(f, "{}: {error}", path.display()),
            Self::Checksum(path, problem) => write!(f, "{}: {problem}", path.display()),
        }
    }
}

impl std::error::Error for SetError {}

/// The newest complete set in `dir`, or `None` when it has none. Sets are tried newest
/// first, so a CHECKSUM older than the newest complete set is never read.
///
/// # Errors
///
/// When `dir` can't be listed, or a CHECKSUM newer than the newest complete set can't be
/// read.
pub fn newest_complete(dir: &Path) -> Result<Option<Set>, SetError> {
    let present = names(dir)?;
    let mut dates: Vec<&str> = present
        .iter()
        .filter(|name| !name.starts_with('.'))
        .filter_map(|name| parse(name))
        .filter(|&(_, what)| what == CHECKSUM)
        .map(|(date, _)| date)
        .collect();
    dates.sort_unstable();
    for date in dates.into_iter().rev() {
        let path = dir.join(checksum_name(date));
        let text = std::fs::read_to_string(&path).map_err(|e| SetError::Io(path.clone(), e))?;
        let checksum = |e| SetError::Checksum(path.clone(), e);
        if is_complete(&present, date, &text).map_err(checksum)? {
            return Ok(Some(Set {
                dir: dir.to_path_buf(),
                date: date.to_string(),
                checksums: checksums(&text).map_err(checksum)?,
            }));
        }
    }
    Ok(None)
}

/// What [`prune`] did.
#[derive(Debug, Default)]
pub struct Pruned {
    /// The files deleted.
    pub deleted: Vec<String>,
    /// The sets left alone because their CHECKSUM can't be read, each with why.
    pub kept: Vec<String>,
}

/// Deletes the sets in `dir` older than the set of `date`, and says what it deleted. Run
/// after the catalog is built from that set, so the folder keeps it and anything newer. A
/// set whose CHECKSUM can't be read or parsed isn't deleted: it is left for the owner, and
/// [`Pruned::kept`] says which and why, so a damaged old CHECKSUM neither hides nor fails
/// the prune after an import that went well.
///
/// # Errors
///
/// When `dir` can't be listed, or a file pruning may delete can't be deleted.
pub fn prune(dir: &Path, date: &str) -> Result<Pruned, SetError> {
    let present = names(dir)?;
    let mut unreadable = Vec::new();
    let mut kept = Vec::new();
    for name in &present {
        let Some(set) = checksum_date(name).filter(|&set| set < date) else {
            continue;
        };
        let path = dir.join(name);
        let read = std::fs::read_to_string(&path)
            .map_err(|error| error.to_string())
            .and_then(|text| checksums(&text));
        if let Err(why) = read {
            kept.push(format!("{}: {why}; the set is kept", path.display()));
            unreadable.push(set);
        }
    }
    let deleted: Vec<String> = prunable(&present, date)
        .into_iter()
        .filter(|name| parse(name).is_none_or(|(set, _)| !unreadable.contains(&set)))
        .collect();
    for name in &deleted {
        let path = dir.join(name);
        std::fs::remove_file(&path).map_err(|e| SetError::Io(path, e))?;
    }
    Ok(Pruned { deleted, kept })
}

/// The names of the files in `dir`. A name that isn't UTF-8 can't be a set's and is left
/// out.
fn names(dir: &Path) -> Result<Vec<String>, SetError> {
    let fail = |error| SetError::Io(dir.to_path_buf(), error);
    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(fail)? {
        if let Ok(name) = entry.map_err(fail)?.file_name().into_string() {
            names.push(name);
        }
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::{Dump, checksums, file_name, is_complete, part_name, prunable};

    const SHA: &str = "4f1c6a0d3b2e5f798a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f6071";

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(ToString::to_string).collect()
    }

    /// A CHECKSUM listing the four dumps of `date`.
    fn listing(date: &str) -> String {
        Dump::ALL.iter().fold(String::new(), |text, &dump| {
            text + SHA + "  " + &file_name(date, dump) + "\n"
        })
    }

    /// Every file of a complete set of `date`.
    fn set(date: &str) -> Vec<String> {
        let mut files: Vec<String> = Dump::ALL.iter().map(|&d| file_name(date, d)).collect();
        files.push(format!("discogs_{date}_CHECKSUM.txt"));
        files
    }

    #[test]
    fn names_are_discogss() {
        assert_eq!(
            file_name("20261101", Dump::Releases),
            "discogs_20261101_releases.xml.gz"
        );
        assert_eq!(
            part_name("discogs_20261101_releases.xml.gz"),
            Some(".discogs_20261101_releases.part".to_string())
        );
        assert_eq!(
            part_name("discogs_20261101_CHECKSUM.txt"),
            Some(".discogs_20261101_CHECKSUM.part".to_string())
        );
        assert_eq!(part_name("notes.txt"), None);
        assert_eq!(
            Dump::ALL.map(Dump::name),
            ["artists", "labels", "masters", "releases"]
        );
    }

    #[test]
    fn reads_a_checksum_file_in_sha256sums_format() {
        let text = format!(
            "{SHA}  discogs_20261101_artists.xml.gz\n{SHA} *discogs_20261101_labels.xml.gz\n\n"
        );
        let listed = checksums(&text).unwrap();
        assert_eq!(
            listed,
            [
                (
                    "discogs_20261101_artists.xml.gz".to_string(),
                    SHA.to_string()
                ),
                (
                    "discogs_20261101_labels.xml.gz".to_string(),
                    SHA.to_string()
                ),
            ]
        );
    }

    #[test]
    fn refuses_a_checksum_line_it_cant_read_and_names_the_line() {
        for (text, line) in [
            ("abc  discogs_20261101_artists.xml.gz\n".to_string(), 1),
            (format!("{SHA}  a\n{}  b\n", SHA.to_uppercase()), 2),
            (format!("{SHA}\n"), 1),
        ] {
            let error = checksums(&text).unwrap_err();
            assert!(error.contains(&format!("line {line}")), "{text}: {error}");
        }
    }

    #[test]
    fn a_set_is_complete_when_its_checksum_lists_the_four_dumps_and_they_exist() {
        let date = "20261101";
        let present = set(date);
        assert!(is_complete(&present, date, &listing(date)).unwrap());
        // A dump still under its temporary name.
        let mut downloading = present.clone();
        downloading.retain(|name| !name.contains("releases"));
        downloading.push(".discogs_20261101_releases.part".to_string());
        assert!(!is_complete(&downloading, date, &listing(date)).unwrap());
        // A CHECKSUM that doesn't list the releases dump.
        let three = listing(date)
            .lines()
            .filter(|line| !line.contains("releases"))
            .fold(String::new(), |text, line| text + line + "\n");
        assert!(!is_complete(&present, date, &three).unwrap());
    }

    #[test]
    fn a_line_beyond_the_four_dumps_doesnt_keep_a_set_incomplete() {
        let date = "20261101";
        let text = format!("{}{SHA}  discogs_{date}_readme.txt\n", listing(date));
        assert!(is_complete(&set(date), date, &text).unwrap());
    }

    #[test]
    fn pruning_deletes_the_sets_older_than_the_one_imported() {
        let mut present = names(&["notes.txt", ".discogs_20261201_artists.part"]);
        for date in ["20260901", "20261001", "20261101"] {
            present.extend(set(date));
        }
        // An incomplete set older than the one imported, and a newer one downloading.
        present.extend(names(&[
            "discogs_20260701_artists.xml.gz",
            ".discogs_20260701_labels.part",
            "discogs_20261201_labels.xml.gz",
        ]));
        let mut deleted = prunable(&present, "20261001");
        deleted.sort();
        let mut expected = set("20260901");
        expected.extend(names(&[
            "discogs_20260701_artists.xml.gz",
            ".discogs_20260701_labels.part",
        ]));
        expected.sort();
        assert_eq!(deleted, expected);
    }

    #[test]
    fn pruning_deletes_nothing_when_no_set_is_older() {
        let mut present = set("20261101");
        present.push("discogs_20261201_artists.xml.gz".to_string());
        assert_eq!(prunable(&present, "20261101"), Vec::<String>::new());
    }
}
