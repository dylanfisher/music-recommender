//! What the service is doing, read at one time: what `sync status` prints and the status
//! page shows, so the two read the same numbers. One section for each part of the service:
//! the community sync, the dumps and their import, and the scores.

use std::fmt;
use std::path::PathBuf;

use catalog::build::{self, State};
use catalog::community::{Progress, Reading};
use catalog::discogs::{Catalog, Summary};
use catalog::score::{self, Computed, Scores};
// The community database, for the API tests, which write their fixtures through it as the
// downloads tests write theirs through `download::Downloads`.
pub use catalog::community::{Community, Counts, Request, Tier, Values};
use config::Config;
use sources::discogs::pacing::{DEFAULT_LIMIT, WINDOW};
use sources::discogs::set::{self, Dump};

use crate::clock::{Clock, unix_seconds};
use crate::import::ImportError;
use crate::rescore::RescoreError;
use crate::sync::{REQUESTS_SPAN, SyncError};

/// The seconds each request takes at the limit the time left is estimated at. Backoffs and
/// waits only add to it.
const SECONDS_PER_REQUEST: u64 = WINDOW.as_secs() / DEFAULT_LIMIT as u64;

/// The requests a minute the time left is estimated at.
pub const REQUESTS_PER_MINUTE: u32 = DEFAULT_LIMIT;

/// Everything the status shows.
#[derive(Debug)]
pub struct Status {
    /// When it was read, in seconds since the Unix epoch.
    pub at: i64,
    /// The community sync.
    pub sync: SyncStatus,
    /// The dumps and their import.
    pub dumps: DumpsStatus,
    /// The scores computed from the catalog and the kept profile.
    pub scores: ScoresStatus,
}

/// The scores' part of the status. Each is the error's text when it can't be read.
#[derive(Debug)]
pub struct ScoresStatus {
    /// Whether a rebuild holds the scores' lock.
    pub running: Result<bool, String>,
    /// What the list in use was computed from and what came of it.
    pub computed: Result<Reading<Computed>, String>,
}

/// The dumps' and the import's part of the status.
#[derive(Debug)]
pub struct DumpsStatus {
    /// `[dumps].dir`, `None` when the config has no `[dumps]` table.
    pub dir: Option<PathBuf>,
    /// The newest complete set's date in `dir`, or why the folder can't be read: a NAS
    /// that isn't mounted is something to show, not a status that fails.
    pub newest_set: Result<Option<String>, String>,
    /// Whether a fetch or an import holds the dumps lock, which they share. Each of these three is the error's text when it
    /// can't be read, shown in the section, so a damaged catalog doesn't hide the sync's.
    pub running: Result<bool, String>,
    /// The catalog in use. Another shape is left alone until a build replaces it.
    pub catalog: Result<Reading<Summary>, String>,
    /// The build in progress, if one was begun and not swapped in.
    pub build: Result<Reading<State>, String>,
}

/// One dump's records, in the build and in the catalog in use.
#[derive(Debug, PartialEq, Eq)]
pub struct DumpRow {
    /// The dump's name.
    pub dump: &'static str,
    /// The records in the build, and whether the dump is loaded whole; `None` without a
    /// build, or before the build reaches this dump.
    pub building: Option<(u64, bool)>,
    /// The records the catalog in use was built with.
    pub in_catalog: Option<u64>,
}

impl DumpsStatus {
    /// The set the build is of, `None` without a build.
    fn build_set(&self) -> Option<&str> {
        let Ok(Reading::Open(state)) = &self.build else {
            return None;
        };
        state
            .imported
            .iter()
            .map(|i| i.date.as_str())
            .chain(state.progress.iter().map(|p| p.date.as_str()))
            .next()
    }

    /// Each dump with a count somewhere, in import order.
    #[must_use]
    pub fn rows(&self) -> Vec<DumpRow> {
        Dump::ALL
            .iter()
            .map(|dump| {
                let name = dump.name();
                let building = match &self.build {
                    Ok(Reading::Open(state)) => state
                        .imported
                        .iter()
                        .find(|i| i.dump == name)
                        .map(|i| (i.records, true))
                        .or_else(|| {
                            state
                                .progress
                                .iter()
                                .find(|p| p.dump == name)
                                .map(|p| (p.records_done, false))
                        }),
                    _ => None,
                };
                let in_catalog = match &self.catalog {
                    Ok(Reading::Open(summary)) => summary
                        .imported
                        .iter()
                        .find(|i| i.dump == name)
                        .map(|i| i.records),
                    _ => None,
                };
                DumpRow {
                    dump: name,
                    building,
                    in_catalog,
                }
            })
            .filter(|row| row.building.is_some() || row.in_catalog.is_some())
            .collect()
    }
}

impl DumpRow {
    /// The row in words, after the dump's name: `2 of 3`, `5 of 5, loaded`, `3 in the
    /// catalog`.
    #[must_use]
    pub fn counts(&self) -> String {
        match (self.building, self.in_catalog) {
            (Some((done, loaded)), last) => {
                let of = last.map_or_else(String::new, |last| format!(" of {last}"));
                let loaded = if loaded { ", loaded" } else { "" };
                format!("{done}{of}{loaded}")
            }
            (None, Some(last)) => format!("{last} in the catalog"),
            (None, None) => String::new(),
        }
    }
}

/// A catalog or a build as found on disk, for a reader that isn't the page or the command
/// and so doesn't want words.
#[derive(Debug, PartialEq, Eq)]
pub enum Found {
    /// No file.
    None,
    /// A file of another shape, left alone.
    OtherShape,
    /// A file in the current shape. `date` is the set it holds, `None` when it holds no dump;
    /// `swapped_at` is when the catalog was swapped in, `None` for a build.
    Present {
        /// The set's date, `YYYYMMDD`.
        date: Option<String>,
        /// When the catalog was swapped in, in seconds since the Unix epoch.
        swapped_at: Option<i64>,
    },
    /// A file that can't be read, with the error's text.
    Unreadable(String),
}

impl DumpsStatus {
    /// The catalog in use as found.
    #[must_use]
    pub fn catalog_found(&self) -> Found {
        match &self.catalog {
            Err(error) => Found::Unreadable(error.clone()),
            Ok(Reading::Missing) => Found::None,
            Ok(Reading::OtherShape) => Found::OtherShape,
            Ok(Reading::Open(summary)) => Found::Present {
                date: summary.imported.first().map(|i| i.date.clone()),
                swapped_at: summary.swapped_at,
            },
        }
    }

    /// The build in progress as found.
    #[must_use]
    pub fn build_found(&self) -> Found {
        match &self.build {
            Err(error) => Found::Unreadable(error.clone()),
            Ok(Reading::Missing) => Found::None,
            Ok(Reading::OtherShape) => Found::OtherShape,
            Ok(Reading::Open(_)) => Found::Present {
                date: self.build_set().map(str::to_string),
                swapped_at: None,
            },
        }
    }
}

impl DumpsStatus {
    /// Whether a fetch or an import is running, in words.
    #[must_use]
    pub fn running_words(&self) -> String {
        match &self.running {
            Ok(true) => "yes".to_string(),
            Ok(false) => "no".to_string(),
            Err(error) => format!("can't be checked: {error}"),
        }
    }
}

/// What the status says of a catalog file of another shape, which it doesn't touch.
const CATALOG_OTHER_SHAPE: &str = "its tables are from another version; the next import \
                                       replaces it";

/// What the status says of a build file of another shape, which it doesn't touch.
const BUILD_OTHER_SHAPE: &str = "its tables are from another version; the next import \
                                     deletes it and starts again";

/// The community sync's part of the status.
#[derive(Debug)]
pub struct SyncStatus {
    /// The counts and state in the community database. `yielding_since` is `None` unless
    /// a sync is running, since a sync killed while yielding leaves it set.
    pub progress: Progress,
    /// Whether a sync holds the sync lock.
    pub running: bool,
    /// The requests sent in the last hour.
    pub requests: u64,
    /// The database file's tables are from another build, so it wasn't read: the counts
    /// are zero, and the sync will delete the file when it next opens it.
    pub other_shape: bool,
}

impl SyncStatus {
    /// The seconds left at `REQUESTS_PER_MINUTE`: a lower bound, since backoffs and waits
    /// only add to it.
    #[must_use]
    pub fn seconds_left(&self) -> u64 {
        let total = self.progress.total();
        (total.queued + total.due) * SECONDS_PER_REQUEST
    }

    /// The time left in words, at `REQUESTS_PER_MINUTE`.
    #[must_use]
    pub fn time_left(&self) -> String {
        let seconds = self.seconds_left();
        let left = match seconds {
            0 => "none".to_string(),
            1..60 => format!("at least {seconds} s"),
            _ => format!("at least {}", words(seconds)),
        };
        format!("{left} at {DEFAULT_LIMIT} a minute")
    }
}

/// What the status says of a database file of another shape, which it doesn't touch.
pub const OTHER_SHAPE: &str = "its tables are from another version; the file will be replaced \
                               when a sync next opens it, and the profile loaded again";

/// The status at the clock's time. It only reads: a missing community database is a sync
/// that hasn't run (all zero), and one of another shape is left for the sync to replace;
/// the catalog and its build are opened read-only, and left as they are, and a failure to
/// read them is shown in the dumps' part rather than failing the status.
///
/// # Errors
///
/// When the community database fails, or the sync lock file can't be checked.
pub fn read(config: &Config, clock: &dyn Clock) -> Result<Status, SyncError> {
    let now = clock.now();
    let at = unix_seconds(now);
    let running = Community::syncing(&config.data_dir)?;
    let (mut progress, requests, other_shape) = match Community::open_read_only(&config.data_dir)? {
        Reading::Missing => (Progress::default(), 0, false),
        Reading::OtherShape => (Progress::default(), 0, true),
        Reading::Open(db) => {
            let since = now.checked_sub(REQUESTS_SPAN).unwrap_or(now);
            let (progress, requests) = db.progress_and_sends(at, since)?;
            (progress, requests, false)
        }
    };
    if !running {
        progress.yielding_since = None;
    }
    Ok(Status {
        at,
        sync: SyncStatus {
            progress,
            running,
            requests,
            other_shape,
        },
        dumps: dumps(config),
        scores: scores(config),
    })
}

/// The scores' part of the status. Nothing here fails it: what can't be read is shown.
fn scores(config: &Config) -> ScoresStatus {
    let data_dir = &config.data_dir;
    let computed = Scores::open_read_only(data_dir).and_then(|reading| {
        Ok(match reading {
            Reading::Missing => Reading::Missing,
            Reading::OtherShape => Reading::OtherShape,
            Reading::Open(scores) => Reading::Open(scores.computed()?),
        })
    });
    ScoresStatus {
        running: score::running(data_dir).map_err(|error| RescoreError::from(error).to_string()),
        computed: computed.map_err(|error| error.to_string()),
    }
}

/// The dumps' part of the status. Nothing here fails it: what can't be read is shown.
fn dumps(config: &Config) -> DumpsStatus {
    let dir = config.dumps.as_ref().map(|dumps| dumps.dir.clone());
    let newest_set = match &dir {
        None => Ok(None),
        Some(dir) => set::newest_complete(dir)
            .map(|found| found.map(|set| set.date))
            .map_err(|error| error.to_string()),
    };
    let data_dir = &config.data_dir;
    let catalog = Catalog::open_read_only(data_dir).and_then(|reading| {
        Ok(match reading {
            Reading::Missing => Reading::Missing,
            Reading::OtherShape => Reading::OtherShape,
            Reading::Open(catalog) => Reading::Open(catalog.summary()?),
        })
    });
    DumpsStatus {
        dir,
        newest_set,
        running: build::running(data_dir).map_err(|error| ImportError::from(error).to_string()),
        catalog: catalog.map_err(|error| error.to_string()),
        build: build::read(data_dir).map_err(|error| error.to_string()),
    }
}

/// What `sync status` prints.
impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sync = &self.sync;
        let p = &sync.progress;
        let yes = |flag| if flag { "yes" } else { "no" };
        writeln!(f, "paused: {}", yes(p.paused))?;
        writeln!(f, "running: {}", yes(sync.running))?;
        if sync.other_shape {
            writeln!(f, "database: {OTHER_SHAPE}")?;
        }
        if let Some(since) = p.yielding_since {
            writeln!(f, "yielding to another client: for {}", self.ago(since))?;
        }
        let parts = [
            ("library", p.library),
            ("candidates", p.candidates),
            ("lists", p.lists),
        ];
        for (name, counts) in parts {
            writeln!(
                f,
                "{name}: {} queued, {} with values, {} failed, {} gone, {} due for refresh",
                counts.queued, counts.with_values, counts.failed, counts.gone, counts.due
            )?;
        }
        if !p.failures.is_empty() {
            writeln!(f, "failed releases, newest first:")?;
            for (release_id, error) in &p.failures {
                writeln!(f, "  release {release_id}: {error}")?;
            }
        }
        writeln!(f, "requests in the last hour: {}", sync.requests)?;
        match &p.last_error {
            Some((at, message)) => writeln!(f, "last error: {} ago: {message}", self.ago(*at))?,
            None => writeln!(f, "last error: none")?,
        }
        writeln!(f, "time left: {}", sync.time_left())?;
        self.write_dumps(f)?;
        write!(
            f,
            "\nscores: {}\nscores rebuilding: {}",
            self.scores_words(),
            self.scores.running_words()
        )
    }
}

impl ScoresStatus {
    /// Whether a rebuild is running, in words.
    #[must_use]
    pub fn running_words(&self) -> String {
        match &self.running {
            Ok(true) => "yes".to_string(),
            Ok(false) => "no".to_string(),
            Err(error) => format!("can't be checked: {error}"),
        }
    }
}

impl Status {
    /// The list in use, in words: `the set of 20261101 and 3952 seeds, computed 5 min ago:
    /// 410233 candidates`.
    #[must_use]
    pub fn scores_words(&self) -> String {
        match &self.scores.computed {
            Err(error) => format!("can't be read: {error}"),
            Ok(Reading::Missing) => "none yet".to_string(),
            Ok(Reading::OtherShape) => SCORES_OTHER_SHAPE.to_string(),
            Ok(Reading::Open(computed)) => format!(
                "the set of {} and {} seeds, computed {} ago: {} candidates",
                computed.inputs.catalog_set,
                computed.seeds,
                self.ago(computed.finished_at),
                computed.candidates
            ),
        }
    }
}

/// What the status says of a scores file of another shape, which it doesn't touch.
const SCORES_OTHER_SHAPE: &str = "its tables are from another version; the next rebuild \
                                  replaces it";

impl Status {
    /// The dumps' lines of what `sync status` prints.
    fn write_dumps(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let dumps = &self.dumps;
        match (&dumps.dir, &dumps.newest_set) {
            (None, _) => writeln!(f, "dumps: off, the config has no [dumps] table")?,
            (Some(dir), Ok(Some(date))) => {
                writeln!(f, "dumps: {}, newest complete set {date}", dir.display())?;
            }
            (Some(dir), Ok(None)) => writeln!(f, "dumps: {}, no complete set", dir.display())?,
            (Some(dir), Err(error)) => writeln!(f, "dumps: {}: {error}", dir.display())?,
        }
        writeln!(f, "fetch or import running: {}", dumps.running_words())?;
        write!(f, "catalog: {}", self.catalog_words())?;
        if let Some(build) = self.build_words() {
            write!(f, "\nbuild: {build}")?;
        }
        for row in dumps.rows() {
            write!(f, "\n  {}: {}", row.dump, row.counts())?;
        }
        Ok(())
    }

    /// The catalog in use, in words: `the set of 20261101, swapped in 2 d 3 h ago`.
    #[must_use]
    pub fn catalog_words(&self) -> String {
        match &self.dumps.catalog {
            Err(error) => format!("can't be read: {error}"),
            Ok(Reading::Missing) => "none yet".to_string(),
            Ok(Reading::OtherShape) => CATALOG_OTHER_SHAPE.to_string(),
            Ok(Reading::Open(summary)) => {
                let date = summary.imported.first().map_or("", |i| i.date.as_str());
                match summary.swapped_at {
                    Some(at) => format!("the set of {date}, swapped in {} ago", self.ago(at)),
                    None => format!("the set of {date}"),
                }
            }
        }
    }

    /// The build in progress, in words, `None` without one.
    #[must_use]
    pub fn build_words(&self) -> Option<String> {
        match &self.dumps.build {
            Err(error) => Some(format!("can't be read: {error}")),
            Ok(Reading::Missing) => None,
            Ok(Reading::OtherShape) => Some(BUILD_OTHER_SHAPE.to_string()),
            Ok(Reading::Open(_)) => Some(format!(
                "the set of {}",
                self.dumps.build_set().unwrap_or("none")
            )),
        }
    }

    /// The time from `then` to when the status was read, in words; a time after it counts
    /// as none.
    #[must_use]
    pub fn ago(&self, then: i64) -> String {
        words(u64::try_from(self.at - then).unwrap_or(0))
    }
}

/// A span of seconds in words, to the minute.
fn words(seconds: u64) -> String {
    let minutes = seconds / 60;
    let (days, hours, minutes) = (minutes / (24 * 60), minutes / 60 % 24, minutes % 60);
    match (days, hours, minutes) {
        (0, 0, 0) => "under a minute".to_string(),
        (0, 0, m) => format!("{m} min"),
        (0, h, m) => format!("{h} h {m} min"),
        (d, h, _) => format!("{d} d {h} h"),
    }
}

#[cfg(test)]
mod tests {
    use super::words;

    #[test]
    fn spans_in_words() {
        assert_eq!(words(59), "under a minute");
        assert_eq!(words(60), "1 min");
        assert_eq!(words(6541), "1 h 49 min");
        assert_eq!(words(46 * 24 * 3600 + 7200 + 59), "46 d 2 h");
    }
}
