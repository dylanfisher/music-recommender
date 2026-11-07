//! The community sync: drains the community database's queue through the Discogs API, one
//! request at a time, committing each reply before the next request (ADR 0004). The same
//! loop runs in a terminal (`sync`) and in the launch agent (`serve`).

use std::fmt;
use std::fs::{File, TryLockError};
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use catalog::community::{Community, CommunityError, Values};
use config::Config;
use sources::discogs::api::{self, Client, Outcome};
use sources::discogs::pacing::Pacer;

use crate::clock::{Clock, unix_seconds};

/// The lock file in the data directory. The syncing process holds an exclusive lock on
/// it, which the OS releases however the process ends.
const LOCK_FILE: &str = "sync.lock";

/// How often `serve` looks for work when the queue is empty, the sync is paused, or a
/// run failed.
const SERVE_INTERVAL: Duration = Duration::from_mins(10);

/// The rate `sync status` estimates the time left at: Discogs's limit a minute.
const REQUESTS_PER_MINUTE: u64 = 60;

/// Why a run of the sync ended without an error.
#[derive(Debug, PartialEq, Eq)]
pub enum Stopped {
    /// The pause flag is set.
    Paused,
    /// Nothing is due.
    Done,
    /// The run sent the number of requests it was limited to.
    Limit,
}

/// How a run went.
#[derive(Debug, PartialEq, Eq)]
pub struct Summary {
    /// Requests sent, retries included.
    pub requests: u64,
    /// Why it ended.
    pub stopped: Stopped,
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let why = match self.stopped {
            Stopped::Paused => "paused (sync resume to continue)",
            Stopped::Done => "nothing left to fetch",
            Stopped::Limit => "reached the limit",
        };
        write!(f, "requests sent: {}; {why}", self.requests)
    }
}

/// Why the sync couldn't run or had to stop.
#[derive(Debug)]
pub enum SyncError {
    /// Another sync holds the lock file.
    Busy(PathBuf),
    /// The lock file can't be opened or locked.
    Lock(PathBuf, io::Error),
    /// The community database failed.
    Database(CommunityError),
    /// Discogs answered 401 or 403. Holds the config file's path and the status.
    TokenRefused(PathBuf, u16),
}

impl fmt::Display for SyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy(path) => write!(
                f,
                "another sync is running (it holds {}); stop it or `sync pause`",
                path.display()
            ),
            Self::Lock(path, error) => write!(f, "{}: {error}", path.display()),
            Self::Database(error) => error.fmt(f),
            Self::TokenRefused(config, status) => write!(
                f,
                "Discogs refused the token (HTTP {status}); check discogs_token in {}",
                config.display()
            ),
        }
    }
}

impl std::error::Error for SyncError {}

impl From<CommunityError> for SyncError {
    fn from(error: CommunityError) -> Self {
        Self::Database(error)
    }
}

/// Runs the sync until it is paused, nothing is due, or it has sent `limit` requests.
/// `log` gets a line for each reply and each wait.
///
/// # Errors
///
/// When another sync is running, the database fails, or Discogs refuses the token.
pub fn run(
    config: &Config,
    clock: &dyn Clock,
    limit: Option<u64>,
    log: &mut dyn FnMut(&str),
) -> Result<Summary, SyncError> {
    let lock_path = config.data_dir.join(LOCK_FILE);
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|error| SyncError::Lock(lock_path.clone(), error))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Err(SyncError::Busy(lock_path)),
        Err(TryLockError::Error(error)) => return Err(SyncError::Lock(lock_path, error)),
    }
    let mut db = Community::open(&config.data_dir)?;
    let client = Client::new(
        config.discogs_api_url.as_deref().unwrap_or(api::BASE_URL),
        config.discogs_token.secret(),
    );
    let mut pacer = Pacer::default();
    let mut requests = 0;
    loop {
        let stopped = |stopped| Ok(Summary { requests, stopped });
        if db.paused()? {
            return stopped(Stopped::Paused);
        }
        if limit.is_some_and(|limit| requests >= limit) {
            return stopped(Stopped::Limit);
        }
        let now = clock.now();
        let Some(release_id) = db.next_due(unix_seconds(now))? else {
            return stopped(Stopped::Done);
        };
        let wait = pacer.wait(now);
        if !wait.is_zero() {
            log(&format!("waiting {} s", wait.as_secs_f64().ceil()));
            clock.sleep(wait);
            // Paused or given more work while waiting: look again.
            continue;
        }
        pacer.sent(now);
        requests += 1;
        let reply = client.release(release_id);
        let now = clock.now();
        pacer.replied(&reply, now);
        let now_seconds = unix_seconds(now);
        let problem = match reply.outcome {
            Outcome::Found(community) => {
                let values = Values {
                    rating_average: community.rating_average,
                    rating_count: community.rating_count,
                    have: community.have,
                    want: community.want,
                };
                db.record(release_id, values, now_seconds)?;
                log(&format!(
                    "release {release_id}: rating {} ({}), have {}, want {}",
                    values.rating_average, values.rating_count, values.have, values.want
                ));
                None
            }
            Outcome::NotFound => {
                db.record_gone(release_id, now_seconds)?;
                log(&format!("release {release_id}: not on Discogs (404)"));
                None
            }
            Outcome::Refused(status) => {
                let error = SyncError::TokenRefused(config.path.clone(), status);
                db.record_error(&error.to_string(), now_seconds)?;
                return Err(error);
            }
            Outcome::RateLimited(_) => {
                log(&format!("release {release_id}: rate-limited (429)"));
                None
            }
            Outcome::ServerError(status) => Some(format!("Discogs answered {status}")),
            Outcome::Network(error) => Some(format!("network: {error}")),
            Outcome::BadReply(error) => Some(format!("bad reply: {error}")),
        };
        if let Some(problem) = problem {
            let message = format!("release {release_id}: {problem}");
            db.record_error(&message, now_seconds)?;
            log(&message);
        }
    }
}

/// The launch agent's loop: runs the sync, and after each run that ends or fails, checks
/// again every 10 minutes. Never returns.
pub fn serve(config: &Config, clock: &dyn Clock, log: &mut dyn FnMut(&str)) -> ! {
    loop {
        match run(config, clock, None, log) {
            Ok(summary) => log(&summary.to_string()),
            Err(error) => log(&format!("error: {error}")),
        }
        clock.sleep(SERVE_INTERVAL);
    }
}

/// Sets or clears the pause flag. A running sync stops after the reply in flight.
///
/// # Errors
///
/// When the database fails.
pub fn set_paused(config: &Config, paused: bool) -> Result<(), SyncError> {
    Community::open(&config.data_dir)?.set_paused(paused)?;
    Ok(())
}

/// What `sync status` shows.
#[derive(Debug)]
pub struct Status {
    progress: catalog::community::Progress,
    now: i64,
}

/// The sync's progress at the clock's time.
///
/// # Errors
///
/// When the database fails.
pub fn status(config: &Config, clock: &dyn Clock) -> Result<Status, SyncError> {
    let now = unix_seconds(clock.now());
    let progress = Community::open(&config.data_dir)?.progress(now)?;
    Ok(Status { progress, now })
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let p = &self.progress;
        writeln!(f, "paused: {}", if p.paused { "yes" } else { "no" })?;
        writeln!(f, "queued: {}", p.queued)?;
        writeln!(f, "fetched: {}", p.fetched)?;
        writeln!(f, "gone: {}", p.gone)?;
        writeln!(f, "due for refresh: {}", p.due)?;
        match &p.last_error {
            Some((at, message)) => {
                let ago = u64::try_from(self.now - at).unwrap_or(0);
                writeln!(f, "last error: {} ago: {message}", words(ago))?;
            }
            None => writeln!(f, "last error: none")?,
        }
        let left = (p.queued + p.due) * 60 / REQUESTS_PER_MINUTE;
        write!(
            f,
            "time left: {} at {REQUESTS_PER_MINUTE} a minute",
            match left {
                0 => "none".to_string(),
                1..60 => words(left),
                _ => format!("about {}", words(left)),
            }
        )
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
