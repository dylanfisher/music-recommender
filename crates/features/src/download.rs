//! The download runner (downloads.md, Search and ranking, Downloading): one loop that searches
//! for the queued jobs, chooses their candidates, and retries the ones not found on a
//! schedule. It is the one writer of the service's states; the API and the commands only
//! create, retry and cancel jobs and record the player's reports.
//!
//! Everything it waits on goes through the injected [`Clock`], so a test runs it against a
//! stub slskd with a made-up clock and nothing sleeps.

use std::fmt;
use std::path::Path;
use std::time::{Duration, SystemTime};

use catalog::community::{LockError, SyncLock};
use catalog::download::{Answer, Downloads, DownloadsError, Event, Job, Stage, State};
use sources::slskd::client::{self as slskd, Client};

use crate::clock::{Clock, unix_seconds};
use crate::query::{SEARCH_TIMEOUT, queries};
use crate::rank::{self, NO_AUDIO, Peer};

/// The least time from one search's start to the next one's, so the account isn't
/// throttled for searching too often.
const SEARCH_GAP: Duration = Duration::from_secs(15);

/// How often a running search is read back while slskd collects responses.
const SEARCH_POLL: Duration = Duration::from_secs(1);

/// How long past its timeout a search may run before it is stopped and read as it is.
const SEARCH_GRACE: Duration = Duration::from_secs(15);

/// How long to wait after slskd answers 429 to a new search, and how many times to try.
const BUSY_BACKOFF: Duration = Duration::from_secs(5);
const BUSY_TRIES: u32 = 6;

/// The wait before each search attempt after the first: 6 hours, a day, 3 days, then
/// weekly.
const RETRY_AFTER: [Duration; 4] = [
    Duration::from_hours(6),
    Duration::from_hours(24),
    Duration::from_hours(3 * 24),
    Duration::from_hours(7 * 24),
];

/// After this many search attempts a job stays `not_found` until the owner retries it.
const MAX_ATTEMPTS: u32 = 8;

/// Why the runner couldn't run, or stopped a round of work.
#[derive(Debug)]
pub enum RunError {
    /// Another runner holds the lock file.
    Busy(std::path::PathBuf),
    /// The lock file can't be opened or locked.
    Lock(std::path::PathBuf, std::io::Error),
    /// The downloads database failed.
    Database(DownloadsError),
    /// slskd refused the key, or is in trouble; the job waits for the next round.
    Slskd(String),
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy(path) => write!(
                f,
                "another download runner is running (it holds {})",
                path.display()
            ),
            Self::Lock(path, error) => write!(f, "{}: {error}", path.display()),
            Self::Database(error) => error.fmt(f),
            Self::Slskd(problem) => write!(f, "slskd: {problem}"),
        }
    }
}

impl std::error::Error for RunError {}

impl From<DownloadsError> for RunError {
    fn from(error: DownloadsError) -> Self {
        Self::Database(error)
    }
}

impl From<LockError> for RunError {
    fn from(error: LockError) -> Self {
        match error {
            LockError::Busy(path) => Self::Busy(path),
            LockError::Io(path, error) => Self::Lock(path, error),
        }
    }
}

/// The words for an slskd error in a log line or a job's reason.
fn words(error: &slskd::Error) -> String {
    match error {
        slskd::Error::Refused(status) => {
            format!("slskd refused the API key (HTTP {status}); check slskd.api_key in the config")
        }
        slskd::Error::Busy => "slskd is busy starting another search (429)".to_string(),
        slskd::Error::NotFound(words) => format!("slskd answered 404: {words}"),
        slskd::Error::Trouble(problem) | slskd::Error::BadReply(problem) => problem.clone(),
    }
}

/// The runner: the downloads store, slskd, the clock, and what it remembers between rounds.
pub struct Runner<'a> {
    store: Downloads,
    client: &'a Client,
    clock: &'a dyn Clock,
    log: &'a mut dyn FnMut(&str),
    /// When the last search started.
    last_search: Option<SystemTime>,
    _lock: SyncLock,
}

impl<'a> Runner<'a> {
    /// Takes the runner's lock in `data_dir`, opens the store, and puts a search a stopped
    /// runner left running back in the queue, as the same attempt.
    ///
    /// # Errors
    ///
    /// When another runner holds the lock, or the database fails.
    pub fn start(
        data_dir: &Path,
        client: &'a Client,
        clock: &'a dyn Clock,
        log: &'a mut dyn FnMut(&str),
    ) -> Result<Self, RunError> {
        let lock = Downloads::lock(data_dir)?;
        let mut runner = Self {
            store: Downloads::open(data_dir)?,
            client,
            clock,
            log,
            last_search: None,
            _lock: lock,
        };
        let now = runner.now();
        for id in runner.store.ids(State::Searching)? {
            let requeued = runner.store.transition(id, Event::Requeue, now)?;
            if runner.settle(id, requeued).is_some() {
                (runner.log)(&format!("job {id}: back in the queue after a restart"));
            }
        }
        Ok(runner)
    }

    /// One round of work: the `not_found` jobs that are due go back in the queue, and the
    /// oldest queued job is searched for.
    ///
    /// # Errors
    ///
    /// When the database fails or slskd can't be used; the job it was on is back in the
    /// queue as the same attempt.
    pub fn tick(&mut self) -> Result<(), RunError> {
        self.requeue_due()?;
        if let Some(&id) = self.store.ids(State::Queued)?.first() {
            self.search(id)?;
        }
        Ok(())
    }

    fn now(&self) -> i64 {
        unix_seconds(self.clock.now())
    }

    /// Puts each `not_found` job whose next attempt is due back in the queue.
    fn requeue_due(&mut self) -> Result<(), RunError> {
        let now = self.now();
        for id in self.store.ids(State::NotFound)? {
            let Some(job) = self.store.job(id, now)? else {
                continue;
            };
            if job.next_attempt_at.is_some_and(|at| at <= now) {
                let requeued = self.store.transition(id, Event::Requeue, now)?;
                self.settle(id, requeued);
            }
        }
        Ok(())
    }

    /// Searches for job `id`: each query in turn, stopping at the first with an accepted
    /// candidate.
    fn search(&mut self, id: i64) -> Result<(), RunError> {
        let searching = self.store.transition(id, Event::Search, self.now())?;
        let Some(job) = self.settle(id, searching) else {
            return Ok(());
        };
        let tracklists = self.store.tracklists(id)?;
        let release = rank::Release {
            artist: &job.release.artist,
            title: &job.release.title,
            year: job.release.year,
            tracklists: &tracklists,
        };
        let mut reason = String::new();
        for text in queries(&job.release.artist, &job.release.title) {
            let peers = match self.query(&text) {
                Ok(peers) => peers,
                Err(error) => return self.search_failed(id, &error),
            };
            match rank::rank(&release, &peers, job.format_policy) {
                Ok(candidates) => {
                    let found = self.store.found(id, &candidates, self.now())?;
                    if let Some(job) = self.settle(id, found) {
                        self.log_job(
                            &job,
                            &format!("found on {} peers' folders", candidates.len()),
                        );
                    }
                    return Ok(());
                }
                // The first query's miss stands unless it found no audio at all.
                Err(miss) => {
                    if reason.is_empty() || reason == NO_AUDIO {
                        reason = miss;
                    }
                }
            }
        }
        let next = next_attempt(job.attempt).map(|after| self.now() + secs(after));
        let not_found = self.store.not_found(id, &reason, next, self.now())?;
        if let Some(job) = self.settle(id, not_found) {
            self.log_job(&job, &format!("not found: {reason}"));
        }
        Ok(())
    }

    /// A search that slskd couldn't run. A reply slskd doesn't send fails the job's search;
    /// anything else puts the job back as the same attempt and stops the round.
    fn search_failed(&mut self, id: i64, error: &slskd::Error) -> Result<(), RunError> {
        let now = self.now();
        if let slskd::Error::BadReply(problem) = error {
            let reason = format!("slskd's search reply: {problem}");
            let failed = self.store.fail(id, Stage::Search, &reason, now)?;
            if self.settle(id, failed).is_some() {
                (self.log)(&format!("job {id}: search failed: {reason}"));
            }
            return Ok(());
        }
        let requeued = self.store.transition(id, Event::Requeue, now)?;
        self.settle(id, requeued);
        Err(RunError::Slskd(words(error)))
    }

    /// Runs one search for `text` and answers the peers' responses: waits out the gap since
    /// the last search, starts it (backing off while slskd is busy), reads it back until
    /// it completes, and deletes it.
    fn query(&mut self, text: &str) -> Result<Vec<Peer>, slskd::Error> {
        if let Some(last) = self.last_search {
            let since = self.clock.now().duration_since(last).unwrap_or_default();
            if let Some(left) = SEARCH_GAP.checked_sub(since).filter(|left| !left.is_zero()) {
                self.clock.sleep(left);
            }
        }
        let mut tries = 0;
        let started = loop {
            self.last_search = Some(self.clock.now());
            match self.client.start_search(text, SEARCH_TIMEOUT) {
                Err(slskd::Error::Busy) if tries + 1 < BUSY_TRIES => {
                    tries += 1;
                    self.clock.sleep(BUSY_BACKOFF);
                }
                other => break other?,
            }
        };
        let began = self.clock.now();
        let mut search = started;
        while !search.complete {
            self.clock.sleep(SEARCH_POLL);
            let running = self.clock.now().duration_since(began).unwrap_or_default();
            if running > SEARCH_TIMEOUT + SEARCH_GRACE {
                self.client.cancel_search(&search.id)?;
                search = self.client.search(&search.id)?;
                break;
            }
            search = self.client.search(&search.id)?;
        }
        self.client.delete_search(&search.id)?;
        Ok(search.responses.into_iter().map(peer).collect())
    }

    /// The job after one of the runner's own changes, or `None` when the store refused it:
    /// the owner cancelled or retried the job meanwhile, which the log says.
    fn settle(&mut self, id: i64, answer: Answer<Job>) -> Option<Job> {
        match answer {
            Ok(job) => Some(job),
            Err(refusal) => {
                (self.log)(&format!(
                    "job {id}: left as it is, it changed meanwhile: {refusal:?}"
                ));
                None
            }
        }
    }

    fn log_job(&mut self, job: &Job, what: &str) {
        (self.log)(&format!(
            "job {} ({} - {}): {what}",
            job.id, job.release.artist, job.release.title
        ));
    }
}

/// The wait before the attempt after attempt `attempt`, or `None` after the last.
fn next_attempt(attempt: u32) -> Option<Duration> {
    if attempt >= MAX_ATTEMPTS {
        return None;
    }
    let index = usize::try_from(attempt.saturating_sub(1)).unwrap_or(usize::MAX);
    Some(RETRY_AFTER[index.min(RETRY_AFTER.len() - 1)])
}

/// A duration in whole seconds, as the store's times are.
fn secs(duration: Duration) -> i64 {
    duration.as_secs().cast_signed()
}

/// A peer's answer as ranking reads it: its locked files with the rest, marked.
fn peer(response: slskd::Response) -> Peer {
    let files = response
        .files
        .into_iter()
        .chain(response.locked_files.into_iter().map(|file| slskd::File {
            locked: true,
            ..file
        }))
        .map(|file| rank::File {
            path: file.filename,
            size: file.size,
            length: file.length,
            bitrate: file.bit_rate,
            bit_depth: file.bit_depth,
            sample_rate: file.sample_rate,
            variable_bitrate: file.variable_bit_rate,
            locked: file.locked,
        })
        .collect();
    Peer {
        username: response.username,
        free_slot: response.has_free_upload_slot,
        queue_length: response.queue_length,
        files,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{MAX_ATTEMPTS, next_attempt};

    #[test]
    fn retries_after_6_hours_a_day_3_days_then_weekly_for_8_attempts() {
        let hours = |attempt| next_attempt(attempt).map(|d: Duration| d.as_secs() / 3600);
        let waits: Vec<_> = (1..=MAX_ATTEMPTS).map(hours).collect();
        assert_eq!(
            waits,
            [
                Some(6),
                Some(24),
                Some(72),
                Some(168),
                Some(168),
                Some(168),
                Some(168),
                None
            ]
        );
    }
}
