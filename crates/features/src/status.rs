//! What the service is doing, read at one time: what `sync status` prints and the status
//! page shows, so the two read the same numbers. One section for each part of the service;
//! today the community sync.

use std::fmt;
use std::time::Duration;

use catalog::community::{Progress, Reading};
// The community database, for the API tests, which write their fixtures through it as the
// downloads tests write theirs through `download::Downloads`.
pub use catalog::community::{Community, Source, Values};
use config::Config;
use sources::discogs::pacing::{DEFAULT_LIMIT, OURS_SPAN, WINDOW};

use crate::clock::{Clock, unix_seconds};
use crate::sync::SyncError;

/// The span the requests count covers. The sync keeps its sends this long.
pub(crate) const REQUESTS_SPAN: Duration = Duration::from_hours(1);

// The sync's pacer reads its sends from the last `OURS_SPAN`, so they must be kept that long.
const _: () = assert!(REQUESTS_SPAN.as_secs() >= OURS_SPAN.as_secs());

/// The seconds each request takes at the limit the time left is estimated at. Backoffs and
/// waits only add to it.
const SECONDS_PER_REQUEST: u64 = WINDOW.as_secs() / DEFAULT_LIMIT as u64;

/// Everything the status shows.
#[derive(Debug)]
pub struct Status {
    /// When it was read, in seconds since the Unix epoch.
    pub at: i64,
    /// The community sync.
    pub sync: SyncStatus,
}

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
    /// The time left in words, at `DEFAULT_LIMIT` a minute: a lower bound, since backoffs
    /// and waits only add to it.
    #[must_use]
    pub fn time_left(&self) -> String {
        let total = self.progress.total();
        let seconds = (total.queued + total.due) * SECONDS_PER_REQUEST;
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
/// that hasn't run (all zero), and one of another shape is left for the sync to replace.
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
            let progress = db.progress(at)?;
            let since = now.checked_sub(REQUESTS_SPAN).unwrap_or(now);
            (progress, db.sends_after(since)?, false)
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
    })
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
        for (name, counts) in [("library", p.library), ("candidates", p.candidates)] {
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
        write!(f, "time left: {}", sync.time_left())
    }
}

impl Status {
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
