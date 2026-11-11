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
//! run starts it again from its first byte. The fetch deletes no set: the import deletes
//! the older ones once the catalog is built from a newer one. While the newest complete
//! set here isn't the catalog's, the fetch downloads nothing, so imports that keep failing
//! leave at most that set and the catalog's on the NAS.

use std::cell::Cell;
use std::fmt;
use std::fs::{self, File};
use std::io::BufWriter;
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
use crate::rescore::Requests;

/// The wait when the site asks for one without saying how long.
const DEFAULT_WAIT: Duration = Duration::from_mins(1);

/// The shortest wait the site's asking is taken as, so a `Retry-After: 0` doesn't ask again
/// at once.
const MIN_WAIT: Duration = Duration::from_secs(5);

/// The longest the fetch waits when asked to. Discogs has asked for 46 minutes; a longer
/// wait is waited this long and the request sent again, and a site that keeps asking uses
/// up `MAX_WAITS`.
const MAX_WAIT: Duration = Duration::from_hours(1);

/// This many waits in one run stop it, whichever requests they were for, so a site that
/// keeps refusing ends the run within about `MAX_WAITS` hours; the next check tries again.
const MAX_WAITS: u32 = 10;

/// Writes to the NAS in pieces this large rather than in the reply's small reads, as large
/// writes suit SMB.
const WRITE_BUFFER: usize = 1 << 20;

/// How long the monthly loop waits after a check that went well: a set appears on the 1st,
/// or mid-January, and is fetched within a day.
const CHECK_EVERY: Duration = Duration::from_hours(24);

/// How long the monthly loop waits after a fetch or an import that failed, and the scores'
/// thread after a rebuild that failed.
pub(crate) const RETRY_AFTER_FAILURE: Duration = Duration::from_hours(1);

/// What a fetch did.
#[derive(Debug)]
pub enum Summary {
    /// The newest complete set here isn't imported yet, so nothing was fetched.
    Waiting {
        /// That set's date.
        pending: String,
    },
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
    },
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Waiting { pending } => write!(
                f,
                "the set of {pending} here isn't imported yet; no fetch until it is"
            ),
            Self::UpToDate { newest } => write!(
                f,
                "no newer set on data.discogs.com than {} here",
                newest.as_deref().unwrap_or("none")
            ),
            Self::Fetched { date, downloaded } => {
                write!(f, "fetched the set of {date}: {} files", downloaded.len())
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
    /// The dumps folder can't be read.
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
/// in `[dumps].dir`, and only when the catalog is from that set: one not imported yet holds
/// the fetch, which sends no request. The listing read is this year's folder, and in January last year's
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
    // A catalog that can't be read isn't from the set either; the import replaces it.
    if let Some(pending) = newest
        .as_ref()
        .filter(|date| !import::built_from(&config.data_dir, date).unwrap_or(false))
    {
        return Ok(Summary::Waiting {
            pending: pending.clone(),
        });
    }
    let (year, month) = year_month(unix_seconds(clock.now()));
    let years: &[i64] = if month == 1 {
        &[year, year - 1]
    } else {
        &[year]
    };
    let waits = Cell::new(0);
    let mut entries = Vec::new();
    for &year in years {
        // A year with no folder yet (in January, before its set) lists nothing; the real
        // site may answer 200 with an empty table or 404, and both are read as empty.
        let page = waiting(
            clock,
            &waits,
            log,
            &format!("the listing of {year}"),
            || match client.listing(year) {
                Fetched::Missing => Fetched::Done(String::new()),
                other => other,
            },
        )?;
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
        waits: &waits,
    };
    for date in dates.into_iter().rev() {
        if let Some(downloaded) = fetch.set(date, log)? {
            return Ok(Summary::Fetched {
                date: date.to_string(),
                downloaded,
            });
        }
    }
    Ok(Summary::UpToDate { newest })
}

/// The monthly loop in `serve`, with `[dumps] monthly = true`: fetch, then import the newest
/// complete set, then wait a day, or an hour after either failed. The import runs even when
/// the fetch failed, so a set fetched earlier, or a finish that failed, is imported at the
/// next check. Each outcome is logged. A catalog swapped in asks `rescore` for the scores'
/// rebuild.
pub fn serve(
    config: &Config,
    client: &Client,
    clock: &dyn Clock,
    rescore: &Requests,
    log: &mut dyn FnMut(&str),
) -> ! {
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
        if let Ok(import::Summary::Built { .. }) = imported {
            rescore.request();
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
    waits: &Cell<u32>,
    log: &mut dyn FnMut(&str),
    what: &str,
    mut request: impl FnMut() -> Fetched<T>,
) -> Result<T, FetchError> {
    loop {
        match request() {
            Fetched::Done(value) => return Ok(value),
            Fetched::Failed(problem) => return Err(FetchError::Refused(problem)),
            Fetched::Missing => {
                return Err(FetchError::Refused(format!(
                    "data.discogs.com has no such file (404) for {what}"
                )));
            }
            Fetched::Wait(_) if waits.get() >= MAX_WAITS => {
                return Err(FetchError::Refused(format!(
                    "data.discogs.com asked to wait {MAX_WAITS} times in this run, the last \
                     before {what}; the next run tries again"
                )));
            }
            Fetched::Wait(asked) => {
                waits.set(waits.get() + 1);
                let wait = asked.unwrap_or(DEFAULT_WAIT).clamp(MIN_WAIT, MAX_WAIT);
                log(&format!(
                    "data.discogs.com asked to wait before {what}: waiting {} s",
                    wait.as_secs()
                ));
                clock.sleep(wait);
            }
        }
    }
}

/// The parts of a fetch that every file uses.
struct Fetch<'a> {
    dir: &'a Path,
    client: &'a Client,
    clock: &'a dyn Clock,
    entries: &'a [Entry],
    /// The waits the site has asked for in this run.
    waits: &'a Cell<u32>,
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
        let listed = fs::read_to_string(&checksum_part)
            .map_err(|error| FetchError::File(checksum_part.clone(), error))
            .and_then(|text| {
                set::checksums(&text)
                    .map_err(|problem| FetchError::Refused(format!("{checksum_name}: {problem}")))
            });
        let listed = match listed {
            Ok(listed) => listed,
            Err(error) => {
                discard(&checksum_part, log);
                return Err(error);
            }
        };
        let dumps_of_set: Vec<String> =
            Dump::ALL.iter().map(|&d| set::file_name(date, d)).collect();
        for (name, _) in listed
            .iter()
            .filter(|(name, _)| !dumps_of_set.contains(name))
        {
            log(&format!(
                "{checksum_name} also lists {name}, which isn't one of the four dumps; it isn't \
                 fetched, and the set is complete without it"
            ));
        }
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
                    discard(&checksum_part, log);
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
        let result = waiting(
            self.clock,
            self.waits,
            log,
            &format!("downloading {name}"),
            || {
                // Each try starts the file again: the site doesn't honor `Range`.
                let failed =
                    |error: std::io::Error| Fetched::Failed(format!("{}: {error}", part.display()));
                let mut file = match File::create(&part) {
                    Ok(file) => BufWriter::with_capacity(WRITE_BUFFER, file),
                    Err(error) => return failed(error),
                };
                match self.client.download(name, entry.size, &mut file) {
                    Fetched::Done(downloaded) => {
                        // Flushed into the file, then synced, before it counts as written.
                        match file
                            .into_inner()
                            .map_err(std::io::IntoInnerError::into_error)
                        {
                            Ok(file) => match file.sync_all() {
                                Ok(()) => Fetched::Done(downloaded),
                                Err(error) => failed(error),
                            },
                            Err(error) => failed(error),
                        }
                    }
                    other => other,
                }
            },
        )
        .and_then(|downloaded| check(name, &downloaded, sha256));
        if let Err(error) = result {
            discard(&part, log);
            return Err(error);
        }
        Ok(part)
    }
}

/// Whether a download is the file the CHECKSUM describes; the client checked its length.
fn check(name: &str, downloaded: &Downloaded, sha256: Option<&str>) -> Result<(), FetchError> {
    match sha256 {
        Some(expected) if downloaded.sha256 != expected => Err(FetchError::Refused(format!(
            "{name}: SHA-256 is {}, but the CHECKSUM file lists {expected}",
            downloaded.sha256
        ))),
        _ => Ok(()),
    }
}

fn rename(from: &Path, to: &Path) -> Result<(), FetchError> {
    fs::rename(from, to).map_err(|error| FetchError::File(from.to_path_buf(), error))
}

/// Deletes a `.part` the fetch is giving up on. One that can't be deleted is logged and
/// left: the error that made the fetch give up is the one to report, and the next run
/// writes the part again from its start.
fn discard(path: &Path, log: &mut dyn FnMut(&str)) {
    match fs::remove_file(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            log(&format!("{}: can't be deleted: {error}", path.display()));
        }
        _ => {}
    }
}
