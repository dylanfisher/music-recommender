//! The fetch: the newest set of dumps on data.discogs.com, downloaded into `[dumps].dir`
//! when it is newer than the newest complete set there, and the monthly loop that fetches
//! and imports in `serve` (ADR 0022).
//!
//! One request at a time. The CHECKSUM comes first, and each dump is written under its
//! hidden `.part` name, checked against its `Content-Length`, its size in the listing and
//! its CHECKSUM line, and only then renamed; the CHECKSUM is renamed last, so a set is
//! never complete, for the import or the player's tagger, before every dump is. A dump
//! already under its real name was checked when it was renamed and isn't downloaded
//! again. A download that fails is deleted, and the site doesn't honor `Range`, so the next
//! run starts it again from its first byte. After a set is complete, the folder is pruned
//! to its two newest complete sets.

use std::fmt;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::Duration;

use catalog::build;
use catalog::community::LockError;
use config::Config;
use primitives::date::year_month;
use sources::discogs::download::{Client, Downloaded, Fetched};
use sources::discogs::listing::{self, Entry};
use sources::discogs::set::{self, Dump, SetError};

use crate::clock::{Clock, unix_seconds};
use crate::import;

/// The wait when the site asks for one without saying how long.
const DEFAULT_WAIT: Duration = Duration::from_mins(1);

/// The shortest wait the site's asking is taken as, so a `Retry-After: 0` doesn't ask again
/// at once.
const MIN_WAIT: Duration = Duration::from_secs(5);

/// This many waits in a row for one request stop the run.
const MAX_WAITS: u32 = 10;

/// How long the monthly loop waits after a check that went well: a set appears on the 1st,
/// or mid-January, and is fetched within a day.
const CHECK_EVERY: Duration = Duration::from_hours(24);

/// How long the monthly loop waits after a fetch or an import that failed.
const RETRY_AFTER_FAILURE: Duration = Duration::from_hours(1);

/// What a fetch did.
#[derive(Debug)]
pub enum Summary {
    /// The site has no set newer than the newest complete one here.
    UpToDate {
        /// That set's date, `None` when the folder has none.
        newest: Option<String>,
    },
    /// A set was fetched.
    Fetched {
        /// Its date.
        date: String,
        /// The files downloaded, the CHECKSUM first.
        downloaded: Vec<String>,
        /// The files pruning deleted.
        pruned: Vec<String>,
    },
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UpToDate { newest } => write!(
                f,
                "no newer set on data.discogs.com than {} here",
                newest.as_deref().unwrap_or("none")
            ),
            Self::Fetched {
                date,
                downloaded,
                pruned,
            } => {
                let pruned = match pruned.len() {
                    0 => "none pruned".to_string(),
                    n => format!("{n} pruned"),
                };
                write!(
                    f,
                    "fetched the set of {date}: {} files, {pruned}",
                    downloaded.len()
                )
            }
        }
    }
}

/// Why a fetch couldn't run or stopped. The catalog in use is never touched by one.
#[derive(Debug)]
pub enum FetchError {
    /// The config has no `[dumps]` table. Holds the config file's path.
    NoDumps(PathBuf),
    /// Another fetch or an import holds the dumps lock.
    Busy(PathBuf),
    /// The lock file can't be opened or locked.
    Lock(PathBuf, std::io::Error),
    /// The dumps folder can't be read or pruned.
    Set(SetError),
    /// A file can't be written, renamed or deleted. Holds its path.
    File(PathBuf, std::io::Error),
    /// The site answered in a way the fetch can't use, or a download didn't check out.
    Refused(String),
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDumps(config) => write!(
                f,
                "{}: no [dumps] table, so there is no folder to fetch dumps into; add one \
                 with `dir`",
                config.display()
            ),
            Self::Busy(path) => write!(
                f,
                "another fetch or import is running (it holds {}); wait for it to end",
                path.display()
            ),
            Self::Lock(path, error) | Self::File(path, error) => {
                write!(f, "{}: {error}", path.display())
            }
            Self::Set(error) => error.fmt(f),
            Self::Refused(problem) => f.write_str(problem),
        }
    }
}

impl std::error::Error for FetchError {}

impl From<LockError> for FetchError {
    fn from(error: LockError) -> Self {
        match error {
            LockError::Busy(path) => Self::Busy(path),
            LockError::Io(path, error) => Self::Lock(path, error),
        }
    }
}

impl From<SetError> for FetchError {
    fn from(error: SetError) -> Self {
        Self::Set(error)
    }
}

/// Fetches the newest set listed on the site when it is newer than the newest complete set
/// in `[dumps].dir`. The listing read is this year's folder, and in January last year's
/// too, since January's set comes out mid-month. Takes the dumps lock, the import's, so a
/// fetch and an import never run at once.
///
/// # Errors
///
/// When the config has no `[dumps]` table, the lock is held, the folder can't be read, the
/// site refuses a request or asks to wait too often, or a download doesn't check out; the
/// download in progress is deleted, and the catalog in use stays.
pub fn run(
    config: &Config,
    client: &Client,
    clock: &dyn Clock,
    log: &mut dyn FnMut(&str),
) -> Result<Summary, FetchError> {
    let dumps = config
        .dumps
        .as_ref()
        .ok_or_else(|| FetchError::NoDumps(config.path.clone()))?;
    let _lock = build::lock(&config.data_dir)?;
    let dir = &dumps.dir;
    let newest = set::newest_complete(dir)?.map(|set| set.date);
    let (year, month) = year_month(unix_seconds(clock.now()));
    let years: &[i64] = if month == 1 {
        &[year, year - 1]
    } else {
        &[year]
    };
    let mut entries = Vec::new();
    for &year in years {
        let page = waiting(clock, log, &format!("the listing of {year}"), || {
            client.listing(year)
        })?;
        entries.extend(listing::parse(&page));
    }
    let mut dates: Vec<&str> = entries
        .iter()
        .filter_map(|entry| set::checksum_date(&entry.name))
        .filter(|date| newest.as_deref().is_none_or(|newest| *date > newest))
        .collect();
    dates.sort_unstable();
    dates.dedup();
    let fetch = Fetch {
        dir,
        client,
        clock,
        entries: &entries,
    };
    for date in dates.into_iter().rev() {
        if let Some(downloaded) = fetch.set(date, log)? {
            let pruned = set::prune(dir)?;
            for name in &pruned {
                log(&format!("pruned {name}"));
            }
            return Ok(Summary::Fetched {
                date: date.to_string(),
                downloaded,
                pruned,
            });
        }
    }
    Ok(Summary::UpToDate { newest })
}

/// The monthly loop in `serve`, with `[dumps] monthly = true`: fetch, then import the newest
/// complete set, then wait a day, or an hour after either failed. The import runs even when
/// the fetch failed, so a set fetched earlier, or a finish that failed, is imported at the
/// next check. Each outcome is logged.
pub fn serve(config: &Config, client: &Client, clock: &dyn Clock, log: &mut dyn FnMut(&str)) -> ! {
    loop {
        let fetched = run(config, client, clock, log);
        match &fetched {
            Ok(summary) => log(&summary.to_string()),
            Err(error) => log(&format!("error: fetch: {error}")),
        }
        let imported = import::run(config, clock, import::BATCH, log);
        match &imported {
            Ok(summary) => log(&summary.to_string()),
            Err(error) => log(&format!("error: import: {error}")),
        }
        clock.sleep(if fetched.is_ok() && imported.is_ok() {
            CHECK_EVERY
        } else {
            RETRY_AFTER_FAILURE
        });
    }
}

/// Sends a request until the site stops asking to wait, waiting each time on `clock`.
fn waiting<T>(
    clock: &dyn Clock,
    log: &mut dyn FnMut(&str),
    what: &str,
    mut request: impl FnMut() -> Fetched<T>,
) -> Result<T, FetchError> {
    for _ in 0..MAX_WAITS {
        match request() {
            Fetched::Done(value) => return Ok(value),
            Fetched::Failed(problem) => return Err(FetchError::Refused(problem)),
            Fetched::Wait(asked) => {
                let wait = asked.unwrap_or(DEFAULT_WAIT).max(MIN_WAIT);
                log(&format!(
                    "data.discogs.com asked to wait before {what}: waiting {} s",
                    wait.as_secs()
                ));
                clock.sleep(wait);
            }
        }
    }
    Err(FetchError::Refused(format!(
        "data.discogs.com asked to wait {MAX_WAITS} times in a row before {what}"
    )))
}

/// The parts of a fetch that every file uses.
struct Fetch<'a> {
    dir: &'a Path,
    client: &'a Client,
    clock: &'a dyn Clock,
    entries: &'a [Entry],
}

impl Fetch<'_> {
    /// Fetches the set of `date`, and returns the files downloaded; `None` when its CHECKSUM
    /// doesn't list the four dumps or the listing lacks one, so an older set is tried.
    fn set(
        &self,
        date: &str,
        log: &mut dyn FnMut(&str),
    ) -> Result<Option<Vec<String>>, FetchError> {
        let checksum_name = set::checksum_name(date);
        let checksum_part = self.download(&checksum_name, None, log)?;
        let text = fs::read_to_string(&checksum_part)
            .map_err(|error| FetchError::File(checksum_part.clone(), error))?;
        let listed = set::checksums(&text)
            .map_err(|problem| FetchError::Refused(format!("{checksum_name}: {problem}")))?;
        let mut dumps = Vec::new();
        for dump in Dump::ALL {
            let name = set::file_name(date, dump);
            let sha256 = listed.iter().find(|(listed, _)| *listed == name);
            match sha256 {
                Some((_, sha256)) if self.entry(&name).is_some() => {
                    dumps.push((name, sha256.clone()));
                }
                _ => {
                    log(&format!(
                        "the set of {date} is skipped: its CHECKSUM or the listing doesn't \
                         list {name}"
                    ));
                    remove(&checksum_part)?;
                    return Ok(None);
                }
            }
        }
        let mut downloaded = vec![checksum_name.clone()];
        for (name, sha256) in &dumps {
            let path = self.dir.join(name);
            if path.exists() {
                log(&format!("{name}: here already"));
                continue;
            }
            let part = self.download(name, Some(sha256), log)?;
            rename(&part, &path)?;
            downloaded.push(name.clone());
        }
        rename(&checksum_part, &self.dir.join(&checksum_name))?;
        log(&format!("the set of {date} is complete"));
        Ok(Some(downloaded))
    }

    /// The listing's entry for `name`.
    fn entry(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.name == name)
    }

    /// Downloads `name` to its `.part` and checks it: its `Content-Length`, its size in the
    /// listing, and `sha256` when there is one. Returns the part's path; a part that doesn't
    /// check out is deleted.
    fn download(
        &self,
        name: &str,
        sha256: Option<&str>,
        log: &mut dyn FnMut(&str),
    ) -> Result<PathBuf, FetchError> {
        let entry = self
            .entry(name)
            .ok_or_else(|| FetchError::Refused(format!("{name}: not in the listing")))?;
        let part = self.dir.join(
            set::part_name(name)
                .ok_or_else(|| FetchError::Refused(format!("{name}: no temporary name for it")))?,
        );
        log(&format!("downloading {name}"));
        let result = waiting(self.clock, log, &format!("downloading {name}"), || {
            // Each try starts the file again: the site doesn't honor `Range`.
            match File::create(&part) {
                Ok(mut file) => match self.client.download(name, &mut file) {
                    Fetched::Done(downloaded) => match file.sync_all() {
                        Ok(()) => Fetched::Done(downloaded),
                        Err(error) => Fetched::Failed(format!("{}: {error}", part.display())),
                    },
                    other => other,
                },
                Err(error) => Fetched::Failed(format!("{}: {error}", part.display())),
            }
        })
        .and_then(|downloaded| check(name, &downloaded, entry, sha256));
        if let Err(error) = result {
            remove(&part)?;
            return Err(error);
        }
        Ok(part)
    }
}

/// Whether a download is the file the listing and the CHECKSUM describe.
fn check(
    name: &str,
    downloaded: &Downloaded,
    entry: &Entry,
    sha256: Option<&str>,
) -> Result<(), FetchError> {
    let refused = |problem: String| Err(FetchError::Refused(format!("{name}: {problem}")));
    let written = downloaded.written;
    match downloaded.content_length {
        None => return refused("the reply had no Content-Length".to_string()),
        Some(length) if length != written => {
            return refused(format!("{written} bytes of a Content-Length of {length}"));
        }
        Some(_) => {}
    }
    if !entry.size.matches(written) {
        return refused(format!("{written} bytes, not the size in the listing"));
    }
    if let Some(expected) = sha256
        && downloaded.sha256 != expected
    {
        return refused(format!(
            "SHA-256 is {}, but the CHECKSUM file lists {expected}",
            downloaded.sha256
        ));
    }
    Ok(())
}

fn rename(from: &Path, to: &Path) -> Result<(), FetchError> {
    fs::rename(from, to).map_err(|error| FetchError::File(from.to_path_buf(), error))
}

fn remove(path: &Path) -> Result<(), FetchError> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(FetchError::File(path.to_path_buf(), error))
        }
        _ => Ok(()),
    }
}
