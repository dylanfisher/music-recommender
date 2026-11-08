//! The download runner (downloads.md, Search and ranking, Downloading): one loop that searches
//! for the queued jobs, chooses their candidates, downloads them through slskd, falls to the
//! next candidate when a peer stalls or fails, and retries the ones not found on a schedule.
//! It is the one writer of the service's states; the API and the commands only create,
//! retry and cancel jobs and record the player's reports.
//!
//! Everything it waits on goes through the injected [`Clock`], so a test runs it against a
//! stub slskd with a made-up clock and nothing sleeps.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use catalog::community::{LockError, SyncLock};
use catalog::download::{Answer, DownloadsError};
// The store's types, for the API and the commands, which reach the store through here.
pub use catalog::download::{
    Candidate, CandidateFile, Chosen, Downloads, Event, File, Format, Handoff, Hold, Job, Listing,
    NewJob, Policy, Progress, Refusal, Release, Source, Stage, State, Tagging, TaggingStatus,
    Track, Upgrade,
};
use config::Config;
use sources::slskd::client::{self as slskd, Client, Transfer, TransferState};

use crate::clock::{Clock, unix_seconds};
use crate::query::{SEARCH_TIMEOUT, queries};
use crate::rank::{self, NO_AUDIO, Peer};
use crate::staging::{self, job_folder, under};

/// How often `serve` runs a round: slskd's transfers are read this often while a job is
/// downloading.
const POLL: Duration = Duration::from_secs(5);

/// How long `serve` waits after a round that slskd or the database failed.
const SERVE_RETRY: Duration = Duration::from_mins(1);

/// The most jobs downloading at once.
const MAX_DOWNLOADING: usize = 4;

/// A file in progress that gains no bytes for this long fails its candidate.
const NO_PROGRESS: Duration = Duration::from_hours(6);

/// A candidate with no file complete this long after it started fails: a peer can hold a
/// file "queued remotely" for days.
const NO_COMPLETED_FILE: Duration = Duration::from_hours(24);

/// A file that fails this many times fails its candidate.
const FILE_FAILURES: u32 = 3;

/// The largest file and album downloaded, in bytes: 2048 MB and 4096 MB.
const MAX_FILE_BYTES: u64 = 2048 * 1024 * 1024;
const MAX_JOB_BYTES: u64 = 4096 * 1024 * 1024;

/// Reads the bytes free on the volume holding a path; [`staging::free_space`] in `serve`.
pub type FreeSpace = dyn Fn(&Path) -> io::Result<u64> + Send + Sync;

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
    /// The config has no `[slskd]` and `[staging]` tables.
    Off(PathBuf),
    /// The downloads database failed.
    Database(DownloadsError),
    /// A folder couldn't be read or written. Holds what was wrong.
    Disk(String),
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
            Self::Off(config) => write!(
                f,
                "the downloads are off: {} has no [slskd] and [staging] tables",
                config.display()
            ),
            Self::Database(error) => error.fmt(f),
            Self::Disk(problem) => f.write_str(problem),
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
    free_space: &'a FreeSpace,
    log: &'a mut dyn FnMut(&str),
    /// `slskd.downloads_dir` and `staging.dir`.
    downloads_dir: PathBuf,
    staging_dir: PathBuf,
    /// When the last search started.
    last_search: Option<SystemTime>,
    /// Each transfer in progress, by slskd's id: its bytes when they last grew, and when.
    /// Kept in memory, so a restart gives a stalled file 6 more hours.
    watched: HashMap<String, (u64, SystemTime)>,
    /// Each job's file failures so far, by the file's remote path, and the transfers
    /// already counted.
    failures: HashMap<(i64, String), u32>,
    counted: HashSet<String>,
    /// Lines about trouble that each round meets again, logged once.
    logged: HashSet<String>,
    _lock: SyncLock,
}

impl<'a> Runner<'a> {
    /// Takes the runner's lock in the data directory, opens the store, and puts a search a
    /// stopped runner left running back in the queue, as the same attempt. Jobs left
    /// enqueueing or downloading carry on: their transfers are adopted, never enqueued
    /// again.
    ///
    /// # Errors
    ///
    /// When the downloads are off, another runner holds the lock, or the database fails.
    pub fn start(
        config: &'a Config,
        client: &'a Client,
        clock: &'a dyn Clock,
        free_space: &'a FreeSpace,
        log: &'a mut dyn FnMut(&str),
    ) -> Result<Self, RunError> {
        let settings = config
            .downloads
            .as_ref()
            .ok_or_else(|| RunError::Off(config.path.clone()))?;
        let lock = Downloads::lock(&config.data_dir)?;
        let mut runner = Self {
            store: Downloads::open(&config.data_dir)?,
            client,
            clock,
            free_space,
            log,
            downloads_dir: settings.slskd_downloads_dir.clone(),
            staging_dir: settings.staging_dir.clone(),
            last_search: None,
            watched: HashMap::new(),
            failures: HashMap::new(),
            counted: HashSet::new(),
            logged: HashSet::new(),
            _lock: lock,
        };
        staging::clear_incoming(&runner.staging_dir).map_err(|error| {
            RunError::Disk(format!(
                "can't clear the copies a stopped runner left in {}: {error}",
                runner.staging_dir.display()
            ))
        })?;
        let now = runner.now();
        for id in runner.store.ids(State::Searching)? {
            let requeued = runner.store.transition(id, Event::Requeue, now)?;
            if runner.settle(id, requeued).is_some() {
                (runner.log)(&format!("job {id}: back in the queue after a restart"));
            }
        }
        Ok(runner)
    }

    /// One round of work: jobs that ended 30 days ago purged; rejected and cancelled jobs'
    /// transfers stopped and their files deleted, and imported jobs' empty folders removed;
    /// downloaded jobs staged; each download read; chosen candidates enqueued, up to 4 jobs downloading; the `not_found`
    /// jobs that are due back in the queue; and the oldest queued job searched for.
    ///
    /// # Errors
    ///
    /// When the database fails, a folder can't be used, or slskd can't be; the job it was
    /// on carries on in the next round.
    pub fn tick(&mut self) -> Result<(), RunError> {
        let purged = self.store.purge(self.now())?;
        if purged > 0 {
            (self.log)(&format!("purged {purged} jobs that ended 30 days ago"));
        }
        self.cleanup()?;
        self.clear_imported()?;
        for id in self.store.ids(State::Downloaded)? {
            self.stage(id)?;
        }
        for id in self.store.ids(State::Downloading)? {
            self.poll(id)?;
        }
        for id in self.store.ids(State::CandidateFound)? {
            if self.store.ids(State::Downloading)?.len() >= MAX_DOWNLOADING {
                break;
            }
            self.enqueue(id)?;
        }
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
                        let what = format!(
                            "{} candidates, the first from {}",
                            candidates.len(),
                            candidates[0].peer
                        );
                        self.log_job(&job, &what);
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

    /// Enqueues the chosen candidate of job `id`, recorded before anything is sent. The
    /// peer's transfers are read first, and files slskd already has (a runner stopped
    /// between the request and its record) are adopted rather than enqueued again, since
    /// slskd would download a second copy under another name.
    fn enqueue(&mut self, id: i64) -> Result<(), RunError> {
        let Some(candidate) = self.store.chosen(id)? else {
            return Ok(());
        };
        let total: u64 = candidate.files.iter().map(|file| file.size).sum();
        if let Some(file) = candidate.files.iter().find(|f| f.size > MAX_FILE_BYTES) {
            let reason = format!("{} is over {} MB", file.name, MAX_FILE_BYTES >> 20);
            return self.fall_back(id, &candidate, &reason);
        }
        if total > MAX_JOB_BYTES {
            let reason = format!("the album is over {} MB", MAX_JOB_BYTES >> 20);
            return self.fall_back(id, &candidate, &reason);
        }
        let free = (self.free_space)(&self.downloads_dir).map_err(|error| {
            RunError::Disk(format!(
                "free space on {}: {error}",
                self.downloads_dir.display()
            ))
        })?;
        if free < total {
            let reason = format!(
                "{} MB free in slskd's downloads folder, and the album needs {} MB",
                free >> 20,
                total.div_ceil(1 << 20)
            );
            let failed = self.store.fail(id, Stage::Download, &reason, self.now())?;
            if let Some(job) = self.settle(id, failed) {
                self.log_job(&job, &format!("download failed: {reason}"));
            }
            return Ok(());
        }
        let transfers = self.transfers(&candidate.peer)?;
        let missing: Vec<(String, u64)> = candidate
            .files
            .iter()
            .filter(|file| !transfers.iter().any(|t| t.filename == file.remote))
            .map(|file| (file.remote.clone(), file.size))
            .collect();
        let adopted = candidate.files.len() - missing.len();
        if !missing.is_empty() {
            match self
                .client
                .enqueue(&candidate.peer, &missing, &job_folder(id))
            {
                Ok(queued) => {
                    if let Some((file, message)) = queued.failures.first() {
                        let reason = format!("slskd didn't queue {file}: {message}");
                        return self.fall_back(id, &candidate, &reason);
                    }
                }
                Err(slskd::Error::NotFound(words)) => {
                    let reason = format!("{} is offline: {words}", candidate.peer);
                    return self.fall_back(id, &candidate, &reason);
                }
                Err(error) => return Err(RunError::Slskd(words(&error))),
            }
        }
        let downloading = self.store.transition(id, Event::Download, self.now())?;
        if let Some(job) = self.settle(id, downloading) {
            let what = format!(
                "downloading {} files from {} ({adopted} already in slskd)",
                candidate.files.len(),
                candidate.peer
            );
            self.log_job(&job, &what);
        }
        Ok(())
    }

    /// Reads job `id`'s transfers: each finished file is found on disk and ledgered, a
    /// failed one is enqueued again until it has failed 3 times, and a candidate whose
    /// file fails 3 times, whose file in progress gains nothing for 6 hours, or with no
    /// file complete 24 hours after it started, falls to the next candidate.
    fn poll(&mut self, id: i64) -> Result<(), RunError> {
        let Some(candidate) = self.store.chosen(id)? else {
            return Ok(());
        };
        let transfers = self.transfers(&candidate.peer)?;
        let ledger: HashSet<String> = self.store.ledger(id)?.into_iter().collect();
        let now = self.clock.now();
        let mut done = 0;
        let mut bytes_done = 0;
        for file in &candidate.files {
            if self
                .located(id, file)
                .is_some_and(|(path, _)| ledger.contains(&path))
            {
                done += 1;
                bytes_done += file.size;
                continue;
            }
            // slskd keeps a file's last transfer at the end of the list.
            let transfer = transfers.iter().rev().find(|t| t.filename == file.remote);
            let problem = match transfer.map(|t| t.state) {
                Some(TransferState::Succeeded) => match self.adopt(id, file)? {
                    Ok(()) => {
                        done += 1;
                        bytes_done += file.size;
                        None
                    }
                    Err(reason) => Some(reason),
                },
                Some(
                    TransferState::Errored
                    | TransferState::Rejected
                    | TransferState::TimedOut
                    | TransferState::Cancelled
                    | TransferState::Aborted,
                )
                | None => self.failed_file(id, &candidate.peer, file, transfer)?,
                Some(TransferState::InProgress) => {
                    let transfer = transfer.expect("a state is a transfer's");
                    bytes_done += transfer.bytes_transferred;
                    self.stalled(transfer, now).then(|| {
                        format!(
                            "{} gained nothing for {} hours",
                            file.name,
                            NO_PROGRESS.as_secs() / 3600
                        )
                    })
                }
                Some(_) => None,
            };
            if let Some(reason) = problem {
                return self.fall_back(id, &candidate, &reason);
            }
        }
        let now = unix_seconds(now);
        if done == candidate.files.len() {
            let downloaded = self.store.transition(id, Event::Downloaded, now)?;
            if let Some(job) = self.settle(id, downloaded) {
                self.log_job(&job, State::Downloaded.name());
            }
            return Ok(());
        }
        let started = self
            .store
            .events(id)?
            .into_iter()
            .rev()
            .find(|(_, state, _)| state == State::Downloading.name())
            .map_or(now, |(at, _, _)| at);
        if done == 0 && now - started >= secs(NO_COMPLETED_FILE) {
            let reason = format!(
                "no file complete in {} hours",
                NO_COMPLETED_FILE.as_secs() / 3600
            );
            return self.fall_back(id, &candidate, &reason);
        }
        let progress = Progress {
            files_done: done as u64,
            files_total: candidate.files.len() as u64,
            bytes_done,
            bytes_total: candidate.files.iter().map(|file| file.size).sum(),
        };
        let progressed = self.store.progress(id, progress, now)?;
        self.settle(id, progressed);
        Ok(())
    }

    /// Whether a transfer in progress has gained no bytes for `NO_PROGRESS`.
    fn stalled(&mut self, transfer: &Transfer, now: SystemTime) -> bool {
        let (bytes, since) = self
            .watched
            .entry(transfer.id.clone())
            .or_insert((transfer.bytes_transferred, now));
        if transfer.bytes_transferred > *bytes {
            *bytes = transfer.bytes_transferred;
            *since = now;
            return false;
        }
        now.duration_since(*since).unwrap_or_default() >= NO_PROGRESS
    }

    /// A file whose transfer failed or is gone from slskd: counted once, then enqueued
    /// again, or the reason its candidate fails after the third time.
    fn failed_file(
        &mut self,
        id: i64,
        peer: &str,
        file: &CandidateFile,
        transfer: Option<&Transfer>,
    ) -> Result<Option<String>, RunError> {
        if let Some(transfer) = transfer
            && !self.counted.insert(transfer.id.clone())
        {
            return Ok(None);
        }
        let count = self.failures.entry((id, file.remote.clone())).or_insert(0);
        *count += 1;
        if *count >= FILE_FAILURES {
            let why = transfer
                .and_then(|t| t.exception.clone())
                .unwrap_or_else(|| "gone from slskd".to_string());
            return Ok(Some(format!(
                "{} failed {FILE_FAILURES} times, the last: {why}",
                file.name
            )));
        }
        if let Some(transfer) = transfer {
            self.cancel_transfer(peer, &transfer.id)?;
        }
        self.client
            .enqueue(peer, &[(file.remote.clone(), file.size)], &job_folder(id))
            .map_err(|error| RunError::Slskd(words(&error)))?;
        Ok(None)
    }

    /// Ledgers a file slskd finished, where it landed, or answers why it can't be.
    fn adopt(&mut self, id: i64, file: &CandidateFile) -> Result<Result<(), String>, RunError> {
        let Some((path, folder)) = self.located(id, file) else {
            return Ok(Err(format!(
                "slskd finished {} but it isn't in {} with its size",
                file.name,
                self.downloads_dir.display()
            )));
        };
        // A file that landed outside the job's folder shares slskd's folder with whatever
        // else came from that peer's folder; a second job there would ledger the first's.
        if !folder.ends_with(&job_folder(id))
            && let Some(other) = self
                .store
                .ledgered_in(&folder)?
                .into_iter()
                .find(|&other| other != id)
        {
            return Ok(Err(format!(
                "{} shares {folder} with job {other}",
                file.name
            )));
        }
        self.store.ledger_add(id, &path)?;
        Ok(Ok(()))
    }

    /// Where slskd put a file of job `id`, if it is there with its size: the job's folder,
    /// or, when slskd didn't honor the destination, a folder named for the peer's. The path
    /// and its folder, canonical.
    fn located(&self, id: i64, file: &CandidateFile) -> Option<(String, String)> {
        let parts: Vec<&str> = file.remote.split(['\\', '/']).collect();
        let name = *parts.last()?;
        let mut folders = vec![job_folder(id)];
        if parts.len() > 1 {
            folders.push(parts[parts.len() - 2].to_string());
        }
        folders.into_iter().find_map(|folder| {
            let path = under(&self.downloads_dir, &Path::new(&folder).join(name)).ok()?;
            let metadata = std::fs::symlink_metadata(&path).ok()?;
            (metadata.is_file() && metadata.len() == file.size).then(|| {
                let folder = path.parent().map(|p| p.display().to_string());
                (path.display().to_string(), folder.unwrap_or_default())
            })
        })
    }

    /// Stops the candidate's transfers, deletes the files it ledgered, and chooses the next
    /// stored candidate. With none left the job searches again as a new attempt, unless
    /// that was the last, when it fails.
    fn fall_back(&mut self, id: i64, candidate: &Candidate, reason: &str) -> Result<(), RunError> {
        self.stop_transfers(candidate)?;
        self.remove_downloads(id)?;
        self.failures.retain(|(job, _), _| *job != id);
        let now = self.now();
        (self.log)(&format!("job {id}: {} failed: {reason}", candidate.peer));
        match self.store.next_candidate(id, now)? {
            Ok(Some(job)) => {
                let next = self.store.chosen(id)?.map(|c| c.peer).unwrap_or_default();
                self.log_job(&job, &format!("trying the next candidate, from {next}"));
            }
            Ok(None) => {
                let attempt = self.store.job(id, now)?.map_or(0, |job| job.attempt);
                let next = if attempt >= MAX_ATTEMPTS {
                    let reason = format!("every candidate failed; the last: {reason}");
                    self.store.fail(id, Stage::Download, &reason, now)?
                } else {
                    self.store.transition(id, Event::Requeue, now)?
                };
                if let Some(job) = self.settle(id, next) {
                    self.log_job(&job, "no candidate left");
                }
            }
            Err(refusal) => {
                self.settle(id, Err(refusal));
            }
        }
        Ok(())
    }

    /// Cancels every transfer slskd has for the candidate's files, and drops its record.
    fn stop_transfers(&mut self, candidate: &Candidate) -> Result<(), RunError> {
        for transfer in self.transfers(&candidate.peer)? {
            if candidate
                .files
                .iter()
                .any(|f| f.remote == transfer.filename)
            {
                self.cancel_transfer(&candidate.peer, &transfer.id)?;
            }
        }
        Ok(())
    }

    fn cancel_transfer(&mut self, peer: &str, id: &str) -> Result<(), RunError> {
        self.watched.remove(id);
        match self.client.cancel_download(peer, id) {
            Ok(()) | Err(slskd::Error::NotFound(_)) => Ok(()),
            Err(error) => Err(RunError::Slskd(words(&error))),
        }
    }

    /// The peer's transfers slskd has a record of; none when it answers 404 for a user
    /// with none.
    fn transfers(&self, peer: &str) -> Result<Vec<Transfer>, RunError> {
        match self.client.downloads(peer) {
            Ok(transfers) => Ok(transfers),
            Err(slskd::Error::NotFound(_)) => Ok(Vec::new()),
            Err(error) => Err(RunError::Slskd(words(&error))),
        }
    }

    /// Deletes each file job `id` ledgered under slskd's downloads folder, and answers
    /// whether all are gone. One that can't be deleted stays ledgered and is logged.
    fn remove_downloads(&mut self, id: i64) -> Result<bool, RunError> {
        let mut all = true;
        for path in self.store.ledger(id)? {
            match staging::remove_ledgered(&self.downloads_dir, &path) {
                Ok(()) => self.store.ledger_remove(id, &path)?,
                Err(error) => {
                    self.log_once(format!("job {id}: can't delete {path}: {error}"));
                    all = false;
                }
            }
        }
        Ok(all)
    }

    /// For each job with `cleanup_pending`: a cancelled job's transfers are stopped, then
    /// its files deleted, slskd's and the staged ones, and the flag cleared once they are
    /// gone. A staged folder holding a file the ledger doesn't keeps the flag, and is logged
    /// once.
    fn cleanup(&mut self) -> Result<(), RunError> {
        let now = self.now();
        for id in self.store.cleanup_pending()? {
            let Some(job) = self.store.job(id, now)? else {
                continue;
            };
            if job.state == State::Cancelled
                && let Some(candidate) = self.store.chosen(id)?
            {
                self.stop_transfers(&candidate)?;
            }
            let downloads_gone = self.remove_downloads(id)?;
            let staged_gone = self.remove_staged(&job, true);
            if downloads_gone && staged_gone {
                let done = self.store.cleanup_done(id, now)?;
                if let Some(job) = self.settle(id, done) {
                    self.log_job(&job, "its files are deleted");
                }
            }
        }
        Ok(())
    }

    /// Copies job `id`'s downloaded files into its staging folder, checks their sizes,
    /// records the staged files, and then deletes slskd's copies. A file missing or of
    /// another size, too little room, or a staging root that isn't mounted fails the stage,
    /// and Retry stages it again.
    fn stage(&mut self, id: i64) -> Result<(), RunError> {
        let Some(candidate) = self.store.chosen(id)? else {
            return Ok(());
        };
        let ledger: HashSet<String> = self.store.ledger(id)?.into_iter().collect();
        let mut sources = Vec::with_capacity(candidate.files.len());
        for file in &candidate.files {
            match self.located(id, file) {
                Some((path, _)) if ledger.contains(&path) => sources.push(staging::Source {
                    path: PathBuf::from(path),
                    name: file.name.clone(),
                    size: file.size,
                }),
                _ => {
                    let reason = format!(
                        "{} is no longer in slskd's downloads folder with its size",
                        file.name
                    );
                    return self.stage_failed(id, &reason);
                }
            }
        }
        let total: u64 = sources.iter().map(|source| source.size).sum();
        if let Ok(free) = (self.free_space)(&self.staging_dir)
            && free < total
        {
            let reason = format!(
                "{} MB free in the staging folder, and the album needs {} MB",
                free >> 20,
                total.div_ceil(1 << 20)
            );
            return self.stage_failed(id, &reason);
        }
        let folder = job_folder(id);
        let names = match staging::stage(&self.staging_dir, &folder, &sources) {
            Ok(names) => names,
            Err(reason) => return self.stage_failed(id, &reason),
        };
        let files: Vec<File> = candidate
            .files
            .iter()
            .zip(names)
            .map(|(file, name)| File {
                name,
                size: file.size,
                position: file.position.clone(),
            })
            .collect();
        let staged = self.store.staged(id, &folder, &files, self.now())?;
        if let Some(job) = self.settle(id, staged) {
            self.log_job(&job, &format!("staged in {folder}"));
            self.remove_downloads(id)?;
        }
        Ok(())
    }

    fn stage_failed(&mut self, id: i64, reason: &str) -> Result<(), RunError> {
        let failed = self.store.fail(id, Stage::Stage, reason, self.now())?;
        if let Some(job) = self.settle(id, failed) {
            self.log_job(&job, &format!("staging failed: {reason}"));
        }
        Ok(())
    }

    /// Deletes a job's staged folder through its ledger, or with `delete_files` false only
    /// the folders an import left empty, and answers whether it is gone. What can't be
    /// deleted is logged once and tried again next round.
    fn remove_staged(&mut self, job: &Job, delete_files: bool) -> bool {
        let Some(folder) = &job.staged_path else {
            return true;
        };
        let names: Vec<String> = job.files.iter().map(|file| file.name.clone()).collect();
        match staging::remove_staged(&self.staging_dir, folder, &names, delete_files) {
            Ok(true) => true,
            Ok(false) => {
                self.log_once(format!(
                    "job {}: {folder} in the staging folder holds files it didn't stage; \
                     they stay",
                    job.id
                ));
                false
            }
            Err(error) => {
                self.log_once(format!(
                    "job {}: can't remove {folder} in the staging folder: {error}",
                    job.id
                ));
                false
            }
        }
    }

    /// Removes each imported job's staged folder once the player has moved every file out.
    fn clear_imported(&mut self) -> Result<(), RunError> {
        let now = self.now();
        for id in self.store.ids(State::Imported)? {
            if let Some(job) = self.store.job(id, now)? {
                self.remove_staged(&job, false);
            }
        }
        Ok(())
    }

    fn log_once(&mut self, line: String) {
        if !self.logged.contains(&line) {
            (self.log)(&line);
            self.logged.insert(line);
        }
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

/// The launch agent's download loop, on its own thread beside the sync: starts the runner
/// and runs a round every 5 s, waiting a minute after a round that failed or a start that
/// couldn't take the lock. Never returns.
pub fn serve(
    config: &Config,
    client: &Client,
    clock: &dyn Clock,
    free_space: &FreeSpace,
    log: &mut dyn FnMut(&str),
) -> ! {
    loop {
        let error = match Runner::start(config, client, clock, free_space, log) {
            Ok(mut runner) => loop {
                match runner.tick() {
                    Ok(()) => clock.sleep(POLL),
                    Err(error) => {
                        (runner.log)(&format!("downloads: error: {error}"));
                        clock.sleep(SERVE_RETRY);
                    }
                }
            },
            Err(error) => error,
        };
        log(&format!("downloads: error: {error}"));
        clock.sleep(SERVE_RETRY);
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
