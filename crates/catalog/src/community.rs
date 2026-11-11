//! The community database: the sync's queue of Discogs release ids, the replies fetched for
//! them with the values scoring reads, and the sync's state (the pause flag, the last
//! error, when it started yielding to another client, its recent sends). Its own file,
//! which the catalog's monthly rebuild never replaces (ADR 0004).
//!
//! Every time is passed in, in seconds since the Unix epoch, so callers and tests choose
//! the clock. Send times are the exception: `record_send` takes a `SystemTime`, kept to the
//! nanosecond.

use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::DatabaseError;
use crate::sqlite;

/// The database's file name inside the data directory.
const FILE_NAME: &str = "community.sqlite";

/// The database's name in its errors.
const NAME: &str = "community";

/// The lock file the process syncing holds, exclusively, so two syncs can't run at once.
const SYNC_LOCK_FILE: &str = "sync.lock";

/// The lock file the process syncing also holds, exclusively, for as long as it holds
/// `SYNC_LOCK_FILE`. `Community::syncing` tries it shared to find out whether a sync runs,
/// so two checks at once don't block each other and a check never touches the lock a sync
/// takes without waiting.
const SYNC_RUNNING_FILE: &str = "sync.running";

/// The lock file `open` holds while it checks, deletes and creates the database file.
const OPEN_LOCK_FILE: &str = "community.open.lock";

/// A value this old is fetched again, behind every release never fetched.
pub const REFRESH_AFTER: Duration = Duration::from_hours(90 * 24);

/// A release is marked failed at its tenth release problem since its last fetch.
const MAX_ATTEMPTS: u64 = 10;

/// After its first release problem a release waits this long, doubling for each one after
/// it...
const RETRY_BASE: Duration = Duration::from_mins(1);

/// ...up to this long.
const RETRY_CAP: Duration = Duration::from_hours(24);

/// `queue` holds every release id ever queued, in order (`seq`, library first). `result` is
/// one row per release fetched, replaced on each fetch: the values read from the reply, or a
/// gone flag. `result_reply` has the reply's JSON as Discogs sent it, about 15 KB, in a table
/// of its own so that scans of `result` (the counts in `progress`) don't read it. A release
/// is never fetched while it has no `result` row: `queue.done` is 1 exactly when it has one,
/// set in the same transaction as the row, so the never-fetched query reads only the
/// releases left. A failed row keeps the values and reply of the last fetch, if any, and
/// adds the last error, which is what marks it failed. `retry` has a row for a release with
/// problems since its last fetch: how many and when it may be tried again. `state` has no
/// column type, so integers stay integers. `send` has a row for each request the sync sent
/// within the retention `record_send` was given of its last, written before the request
/// goes out: its time in nanoseconds since the Unix epoch, since the pacer compares sends a
/// fraction of a second apart.
///
/// `queue_todo` holds only the releases not done, in the order `next_due` wants them, so a
/// request reads the first one without sorting or passing over those already fetched.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS queue (
    seq INTEGER PRIMARY KEY,
    release_id INTEGER NOT NULL UNIQUE,
    source INTEGER NOT NULL,
    done INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS queue_todo ON queue (source, seq) WHERE done = 0;
CREATE TABLE IF NOT EXISTS result (
    release_id INTEGER PRIMARY KEY REFERENCES queue (release_id),
    fetched_at INTEGER NOT NULL,
    gone INTEGER NOT NULL,
    error TEXT,
    rating_average REAL,
    rating_count INTEGER,
    have INTEGER,
    want INTEGER,
    num_for_sale INTEGER,
    lowest_price REAL
);
CREATE INDEX IF NOT EXISTS result_fetched_at ON result (fetched_at) WHERE gone = 0;
CREATE TABLE IF NOT EXISTS result_reply (
    release_id INTEGER PRIMARY KEY REFERENCES queue (release_id),
    reply TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS retry (
    release_id INTEGER PRIMARY KEY REFERENCES queue (release_id),
    attempts INTEGER NOT NULL,
    retry_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS state (
    key TEXT PRIMARY KEY,
    value NOT NULL
);
CREATE TABLE IF NOT EXISTS send (
    sent_ns INTEGER NOT NULL
);
";

/// The first release never fetched whose retry time, if it has one, has passed. `?1` is the
/// time now.
const NEVER_FETCHED: &str = "SELECT q.release_id FROM queue q WHERE q.done = 0
     AND NOT EXISTS (SELECT 1 FROM retry t WHERE t.release_id = q.release_id AND t.retry_at > ?1)
     ORDER BY q.source, q.seq LIMIT 1";

/// The release with the oldest values among those due for a refresh whose retry time, if it
/// has one, has passed. `?1` is the cutoff for the values' age, `?2` the time now.
const REFRESH: &str = "SELECT r.release_id FROM result r
     WHERE r.gone = 0 AND r.fetched_at <= ?1
     AND NOT EXISTS (SELECT 1 FROM retry t WHERE t.release_id = r.release_id AND t.retry_at > ?2)
     ORDER BY r.fetched_at, r.release_id LIMIT 1";

const PAUSED: &str = "paused";
/// How many failed releases `progress` lists.
const FAILED_SHOWN: usize = 10;

const LAST_ERROR: &str = "last_error";
const LAST_ERROR_AT: &str = "last_error_at";
const YIELDING_SINCE: &str = "yielding_since";

/// Where a queued release id came from. The library's are fetched before candidates'.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// A release the owner has, from the profile.
    Library = 0,
    /// A release a recommendation list suggests.
    Candidate = 1,
}

/// The values scoring reads from a release's reply, under the API's names.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Values {
    /// `community.rating.average`.
    pub rating_average: f64,
    /// `community.rating.count`.
    pub rating_count: u64,
    /// `community.have`.
    pub have: u64,
    /// `community.want`.
    pub want: u64,
    /// `num_for_sale`, `None` when the reply has none or it isn't a whole number.
    pub num_for_sale: Option<u64>,
    /// `lowest_price`, `None` when the reply has none, it is null or it isn't a number.
    pub lowest_price: Option<f64>,
}

/// What the database holds for a release id, as `stored` reads it. Each time is when the
/// sync stored the answer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Stored {
    /// Not in the queue.
    NotQueued,
    /// Queued and never fetched, possibly waiting to be tried again after a release problem.
    Queued,
    /// Fetched.
    Fetched {
        /// The values read from the reply.
        values: Values,
        /// When it was fetched.
        at: i64,
    },
    /// Given up on after `MAX_ATTEMPTS` release problems. Fetched again at the refresh.
    Failed {
        /// The values of an earlier fetch, if it had one.
        values: Option<Values>,
        /// When it was given up on.
        at: i64,
    },
    /// Answered 404: no longer on Discogs.
    Gone {
        /// When the 404 came.
        at: i64,
    },
}

/// What a release problem did to a release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The release will be tried again.
    Retry,
    /// That was the last allowed: the release is marked failed and comes due again at the
    /// refresh.
    Failed,
}

/// One source's releases, as `progress` counts them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// Queued and never fetched.
    pub queued: u64,
    /// With values from a fetch, including failed releases that had some.
    pub with_values: u64,
    /// Given up on after `MAX_ATTEMPTS` release problems. Values from an earlier fetch
    /// are kept. Fetched again on refresh.
    pub failed: u64,
    /// Fetched and answered 404: no longer on Discogs. These aren't fetched again.
    pub gone: u64,
    /// Fetched or failed, at a time older than `REFRESH_AFTER`.
    pub due: u64,
}

/// Counts and state for the status: `sync status` and the status page.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Progress {
    /// The library's releases.
    pub library: Counts,
    /// The candidates' releases.
    pub candidates: Counts,
    /// The pause flag.
    pub paused: bool,
    /// The last error recorded and its time, if any.
    pub last_error: Option<(i64, String)>,
    /// When the running sync started yielding to another client of the Discogs API (the
    /// player's tagger), if it is yielding. A sync killed while yielding leaves it set, so
    /// it means something only while a sync holds the lock.
    pub yielding_since: Option<i64>,
    /// The `FAILED_SHOWN` most recent failed releases, newest first, with their last error.
    pub failures: Vec<(u64, String)>,
}

impl Progress {
    /// The library's and the candidates' counts added together.
    #[must_use]
    pub fn total(&self) -> Counts {
        let (a, b) = (self.library, self.candidates);
        Counts {
            queued: a.queued + b.queued,
            with_values: a.with_values + b.with_values,
            failed: a.failed + b.failed,
            gone: a.gone + b.gone,
            due: a.due + b.due,
        }
    }
}

/// Why the sync lock couldn't be taken.
#[derive(Debug)]
pub enum LockError {
    /// Another process holds it. Holds the lock file's path.
    Busy(PathBuf),
    /// The lock file can't be opened or locked.
    Io(PathBuf, std::io::Error),
}

/// A process lock, the sync's or the download runner's: the OS releases it when this drops
/// or the process ends however it ends.
#[derive(Debug)]
pub struct SyncLock {
    _file: File,
    /// The sync's second lock, `SYNC_RUNNING_FILE`; none for the download locks.
    _running: Option<File>,
}

/// Takes an exclusive lock on the file at `path`, creating it, without waiting.
pub(crate) fn try_lock(path: PathBuf) -> Result<SyncLock, LockError> {
    let file = open_lock_file(&path)?;
    match file.try_lock() {
        Ok(()) => Ok(SyncLock {
            _file: file,
            _running: None,
        }),
        Err(TryLockError::WouldBlock) => Err(LockError::Busy(path)),
        Err(TryLockError::Error(error)) => Err(LockError::Io(path, error)),
    }
}

/// Takes the lock a process holds while it runs, on the pattern of ADR 0021: an exclusive
/// lock on `lock` without waiting, so a second run is refused at once, and, while it holds
/// that, an exclusive lock on `running`, which `running` probes. Only a probe, held for
/// microseconds, can be in the way of the second, so it waits for it instead of failing.
pub(crate) fn lock_running(lock: PathBuf, running: PathBuf) -> Result<SyncLock, LockError> {
    hold_running(try_lock(lock)?, running)
}

/// `lock`, now also holding the exclusive lock on `running`, waiting for a probe.
fn hold_running(lock: SyncLock, running: PathBuf) -> Result<SyncLock, LockError> {
    let file = open_lock_file(&running)?;
    file.lock().map_err(|error| LockError::Io(running, error))?;
    Ok(SyncLock {
        _running: Some(file),
        ..lock
    })
}

/// `lock_running` for work that waits its turn instead of being refused: while another
/// process holds `lock`, `waiting` is called once and the lock waited for.
pub(crate) fn lock_running_waiting(
    lock: PathBuf,
    running: PathBuf,
    waiting: impl FnOnce(),
) -> Result<SyncLock, LockError> {
    let lock = match try_lock(lock) {
        Err(LockError::Busy(path)) => {
            waiting();
            let file = open_lock_file(&path)?;
            file.lock().map_err(|error| LockError::Io(path, error))?;
            SyncLock {
                _file: file,
                _running: None,
            }
        }
        taken => taken?,
    };
    hold_running(lock, running)
}

/// Whether a run holds the lock file `path` that `lock_running` takes second, found without
/// waiting and without creating the file. The probe is shared and let go at once, so probes
/// never see each other as a run.
pub(crate) fn running(path: PathBuf) -> Result<bool, LockError> {
    let file = match File::open(&path) {
        Ok(file) => file,
        // `lock_running` creates it, so no run has been here.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(LockError::Io(path, error)),
    };
    match file.try_lock_shared() {
        Ok(()) => Ok(false),
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(error)) => Err(LockError::Io(path, error)),
    }
}

/// Opens the lock file at `path`, creating it.
fn open_lock_file(path: &Path) -> Result<File, LockError> {
    File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(|error| LockError::Io(path.to_path_buf(), error))
}

pub use crate::sqlite::Reading;

/// Where the community database is and what it holds.
fn spec() -> sqlite::Spec<'static> {
    static EXPECTED: sqlite::Expected = sqlite::Expected::new();
    sqlite::Spec {
        file_name: FILE_NAME,
        lock_file: OPEN_LOCK_FILE,
        schema: SCHEMA,
        later: "",
        expected: &EXPECTED,
    }
}

/// A failed open of the file at `path`.
fn open_failure(path: &Path, error: sqlite::OpenError) -> DatabaseError {
    DatabaseError::new(NAME, path, error.boxed())
}

/// An open community database.
pub struct Community {
    path: PathBuf,
    connection: Connection,
}

impl Community {
    /// Takes the exclusive lock that the one process syncing holds, on `sync.lock` in
    /// `data_dir`, so two syncs can't send requests at once (ADR 0004), without waiting.
    /// While it holds that it also takes an exclusive lock on `sync.running`, which
    /// `syncing` probes. Only a check's shared probe, held for microseconds, can be in the
    /// way of that one, so it waits for the probe instead of failing.
    ///
    /// # Errors
    ///
    /// When another process holds it, or a lock file can't be opened or locked.
    pub fn lock(data_dir: &Path) -> Result<SyncLock, LockError> {
        lock_running(
            data_dir.join(SYNC_LOCK_FILE),
            data_dir.join(SYNC_RUNNING_FILE),
        )
    }

    /// Whether a sync is running in `data_dir`: whether `sync.running` is locked, found
    /// without waiting and without creating the file. The check takes the lock shared and
    /// lets go at once, so checks never see each other as a sync, and a sync's `lock`
    /// waits for that moment instead of failing.
    ///
    /// # Errors
    ///
    /// When the lock file exists but can't be opened or locked.
    pub fn syncing(data_dir: &Path) -> Result<bool, LockError> {
        running(data_dir.join(SYNC_RUNNING_FILE))
    }

    /// Opens the community database in `data_dir`, creating the file and its tables if
    /// they don't exist. The directory itself must exist: a misspelled `data_dir` fails
    /// here instead of starting an empty queue somewhere else.
    ///
    /// A file whose tables and indexes aren't `SCHEMA`'s (an older build wrote it) is
    /// deleted, with its `-wal` and `-shm`, and created again empty, the queue too, since
    /// the profile can be loaded again; a line saying so goes to stderr. The check, the
    /// delete and the create run under an exclusive lock on `community.open.lock`, so two
    /// processes opening at once end with one fresh file. Waiting for that lock gives up
    /// after `BUSY_TIMEOUT`.
    ///
    /// # Errors
    ///
    /// When `data_dir` isn't a directory, the lock can't be taken in time, or the file can't
    /// be opened or set up.
    pub fn open(data_dir: &Path) -> Result<Self, DatabaseError> {
        Self::open_waiting(data_dir, sqlite::BUSY_TIMEOUT)
    }

    /// `open`, waiting at most `lock_timeout` for the open lock.
    fn open_waiting(data_dir: &Path, lock_timeout: Duration) -> Result<Self, DatabaseError> {
        let path = data_dir.join(FILE_NAME);
        let opened = sqlite::open(data_dir, &spec(), lock_timeout)
            .map_err(|error| open_failure(&path, error))?;
        if opened.reset {
            eprintln!(
                "community database {}: its tables are from another version, so the file was \
                 deleted and started again; load the profile again",
                path.display()
            );
        }
        Ok(Self {
            path,
            connection: opened.connection,
        })
    }

    /// Opens the community database in `data_dir` to read, and only read: it creates no
    /// file, deletes none and doesn't reset a file of another shape (see `Reading`), so a
    /// status check changes nothing a sync will find. Waiting for `community.open.lock`
    /// gives up after `BUSY_TIMEOUT`. The writing methods fail on this connection.
    ///
    /// # Errors
    ///
    /// When `data_dir` isn't a directory, the lock can't be taken in time, or the file can't
    /// be opened.
    pub fn open_read_only(data_dir: &Path) -> Result<Reading<Self>, DatabaseError> {
        let path = data_dir.join(FILE_NAME);
        let reading = sqlite::open_read_only(data_dir, &spec(), sqlite::BUSY_TIMEOUT)
            .map_err(|error| open_failure(&path, error))?;
        Ok(reading.map(|connection| Self { path, connection }))
    }

    /// Adds release ids to the queue, in order, and returns how many weren't queued
    /// before. An id already queued keeps its place; one queued as a candidate and now
    /// in the library moves up to the library's.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn queue(&mut self, release_ids: &[u64], source: Source) -> Result<u64, DatabaseError> {
        let fail = |error| failure(&self.path, error);
        let source = source as i64;
        let transaction = self.connection.transaction().map_err(fail)?;
        let mut added = 0;
        {
            let mut insert = transaction
                .prepare("INSERT OR IGNORE INTO queue (release_id, source) VALUES (?1, ?2)")
                .map_err(fail)?;
            let mut promote = transaction
                .prepare("UPDATE queue SET source = ?2 WHERE release_id = ?1 AND source > ?2")
                .map_err(fail)?;
            for &id in release_ids {
                if insert.execute(params![id, source]).map_err(fail)? == 0 {
                    promote.execute(params![id, source]).map_err(fail)?;
                } else {
                    added += 1;
                }
            }
        }
        transaction.commit().map_err(fail)?;
        Ok(added)
    }

    /// The release to fetch next at time `now`: the first never fetched, library before
    /// candidates and each in `seq` order; then the one whose values are oldest, if they're
    /// `REFRESH_AFTER` old. A release with problems on record is skipped until its retry
    /// time. `None` when nothing is due.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn next_due(&self, now: i64) -> Result<Option<u64>, DatabaseError> {
        let fail = |error| failure(&self.path, error);
        let never = self.first(NEVER_FETCHED, [now]).map_err(fail)?;
        if never.is_some() {
            return Ok(never);
        }
        self.first(REFRESH, [refresh_cutoff(now), now])
            .map_err(fail)
    }

    /// Records the reply fetched for a release at time `now`, its JSON as Discogs sent it
    /// and the values read from it, replacing older ones.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn record(
        &mut self,
        release_id: u64,
        values: Values,
        reply: &str,
        now: i64,
    ) -> Result<(), DatabaseError> {
        self.replace_result(
            release_id,
            Some(reply),
            "INSERT OR REPLACE INTO result
             (release_id, fetched_at, gone, rating_average, rating_count, have, want,
              num_for_sale, lowest_price)
             VALUES (?1, ?2, 0, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                release_id,
                now,
                values.rating_average,
                values.rating_count,
                values.have,
                values.want,
                values.num_for_sale,
                values.lowest_price
            ],
        )
    }

    /// The reply last stored for a release, as Discogs sent it. `None` when it has none:
    /// never fetched, or gone.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn reply(&self, release_id: u64) -> Result<Option<String>, DatabaseError> {
        self.first(
            "SELECT reply FROM result_reply WHERE release_id = ?1",
            [release_id],
        )
        .map_err(|error| failure(&self.path, error))
    }

    /// What the database holds for each of `release_ids`, in their order, read in one
    /// transaction so the answers agree with each other while a sync writes.
    ///
    /// # Errors
    ///
    /// When a read fails, or a fetched release has no values.
    pub fn stored(&self, release_ids: &[u64]) -> Result<Vec<Stored>, DatabaseError> {
        let fail = |error| failure(&self.path, error);
        self.read(|| {
            let mut statement = self
                .connection
                .prepare_cached(
                    "SELECT r.fetched_at, r.gone, r.error IS NOT NULL, r.rating_average,
                            r.rating_count, r.have, r.want, r.num_for_sale, r.lowest_price
                     FROM queue q LEFT JOIN result r ON r.release_id = q.release_id
                     WHERE q.release_id = ?1",
                )
                .map_err(fail)?;
            release_ids
                .iter()
                .map(|&id| {
                    statement
                        .query_row([id], stored_row)
                        .optional()
                        .map(|row| row.unwrap_or(Stored::NotQueued))
                        .map_err(fail)
                })
                .collect()
        })
    }

    /// Records that the API answered 404 for a release at time `now`. Its old reply and
    /// values, if any, are dropped.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn record_gone(&mut self, release_id: u64, now: i64) -> Result<(), DatabaseError> {
        self.replace_result(
            release_id,
            None,
            "INSERT OR REPLACE INTO result (release_id, fetched_at, gone) VALUES (?1, ?2, 1)",
            params![release_id, now],
        )
    }

    /// Counts a release problem for a release and sets the last error naming the release,
    /// in one transaction. The release waits to be tried again: a minute after the first,
    /// doubling for each one after it, to a day. At the `MAX_ATTEMPTS`th it is marked failed
    /// instead, keeping the reply and values of an earlier fetch, and comes due at the refresh.
    /// `record` and `record_gone` clear the count. Returns what happened and the last error's
    /// text, for the log.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn record_failure(
        &mut self,
        release_id: u64,
        problem: &str,
        now: i64,
    ) -> Result<(Failure, String), DatabaseError> {
        let fail = |error| failure(&self.path, error);
        let transaction = self.connection.transaction().map_err(fail)?;
        let attempts: u64 = transaction
            .prepare_cached("SELECT attempts FROM retry WHERE release_id = ?1")
            .and_then(|mut statement| {
                statement
                    .query_row([release_id], |row| row.get(0))
                    .optional()
            })
            .map_err(fail)?
            .unwrap_or(0)
            + 1;
        let outcome = if attempts >= MAX_ATTEMPTS {
            mark_failed(&transaction, release_id, problem, now).map_err(fail)?;
            Failure::Failed
        } else {
            transaction
                .prepare_cached(
                    "INSERT OR REPLACE INTO retry (release_id, attempts, retry_at)
                     VALUES (?1, ?2, ?3)",
                )
                .and_then(|mut statement| {
                    statement.execute(params![
                        release_id,
                        attempts,
                        now + retry_wait(attempts).as_secs().cast_signed()
                    ])
                })
                .map_err(fail)?;
            Failure::Retry
        };
        let message = release_error(release_id, problem);
        set_last_error(&transaction, &message, now).map_err(fail)?;
        transaction.commit().map_err(fail)?;
        Ok((outcome, message))
    }

    /// Sets the last error naming the release, for a reply that leaves the release as it
    /// was (a 429, trouble with Discogs or the network). Returns its text, for the log.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn record_release_error(
        &mut self,
        release_id: u64,
        problem: &str,
        now: i64,
    ) -> Result<String, DatabaseError> {
        let message = release_error(release_id, problem);
        self.record_error(&message, now)?;
        Ok(message)
    }

    /// Replaces a release's `result` row and its reply, or drops the reply when `reply` is
    /// `None`, and clears its retry row, together.
    fn replace_result(
        &mut self,
        release_id: u64,
        reply: Option<&str>,
        sql: &str,
        values: impl rusqlite::Params,
    ) -> Result<(), DatabaseError> {
        let fail = |error| failure(&self.path, error);
        let transaction = self.connection.transaction().map_err(fail)?;
        transaction.execute(sql, values).map_err(fail)?;
        match reply {
            Some(reply) => transaction.execute(
                "INSERT OR REPLACE INTO result_reply (release_id, reply) VALUES (?1, ?2)",
                params![release_id, reply],
            ),
            None => transaction.execute(
                "DELETE FROM result_reply WHERE release_id = ?1",
                [release_id],
            ),
        }
        .map_err(fail)?;
        transaction
            .execute("DELETE FROM retry WHERE release_id = ?1", [release_id])
            .map_err(fail)?;
        mark_done(&transaction, release_id).map_err(fail)?;
        transaction.commit().map_err(fail)
    }

    /// Records an error of the run for `sync status`, replacing the previous one.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn record_error(&mut self, message: &str, now: i64) -> Result<(), DatabaseError> {
        let fail = |error| failure(&self.path, error);
        let transaction = self.connection.transaction().map_err(fail)?;
        set_last_error(&transaction, message, now).map_err(fail)?;
        transaction.commit().map_err(fail)
    }

    /// Records a request about to go out at `at`, and drops the sends `keep` or more before
    /// it, so a new run's pacer starts with this service's recent sends (ADR 0020). Call it
    /// just before sending: Discogs may count a request whose reply never comes back.
    ///
    /// # Errors
    ///
    /// When `at` is before the Unix epoch or the write fails.
    pub fn record_send(&mut self, at: SystemTime, keep: Duration) -> Result<(), DatabaseError> {
        let fail = |error| failure(&self.path, error);
        let at = epoch_nanos(at).map_err(fail)?;
        let transaction = self.connection.transaction().map_err(fail)?;
        transaction
            .prepare_cached("INSERT INTO send (sent_ns) VALUES (?1)")
            .and_then(|mut statement| statement.execute([at]))
            .map_err(fail)?;
        let oldest_kept = at.saturating_sub(nanos(keep).map_err(fail)?);
        transaction
            .prepare_cached("DELETE FROM send WHERE sent_ns <= ?1")
            .and_then(|mut statement| statement.execute([oldest_kept]))
            .map_err(fail)?;
        transaction.commit().map_err(fail)
    }

    /// Sets or clears the pause flag, which every running sync reads before each request.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn set_paused(&self, paused: bool) -> Result<(), DatabaseError> {
        self.connection
            .execute(SET_STATE, params![PAUSED, paused])
            .map(drop)
            .map_err(|error| failure(&self.path, error))
    }

    /// Records the time the sync started yielding to another client of the Discogs API, or
    /// clears it when `None`: when the sync sends normally again or its run ends.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn set_yielding(&self, since: Option<i64>) -> Result<(), DatabaseError> {
        match since {
            Some(at) => self
                .connection
                .execute(SET_STATE, params![YIELDING_SINCE, at]),
            None => self
                .connection
                .execute("DELETE FROM state WHERE key = ?1", [YIELDING_SINCE]),
        }
        .map(drop)
        .map_err(|error| failure(&self.path, error))
    }

    /// The sends the sync recorded after `after`, oldest first. One the clock now puts in
    /// the future (it stepped back) is among them.
    ///
    /// # Errors
    ///
    /// When `after` is before the Unix epoch or the read fails.
    pub fn recent_sends(&self, after: SystemTime) -> Result<Vec<SystemTime>, DatabaseError> {
        let fail = |error| failure(&self.path, error);
        self.connection
            .prepare_cached("SELECT sent_ns FROM send WHERE sent_ns > ?1 ORDER BY sent_ns")
            .map_err(fail)?
            .query_map([epoch_nanos(after).map_err(fail)?], |row| {
                Ok(SystemTime::UNIX_EPOCH + Duration::from_nanos(row.get(0)?))
            })
            .map_err(fail)?
            .collect::<rusqlite::Result<_>>()
            .map_err(fail)
    }

    /// How many sends the sync recorded after `after`. The table holds only what
    /// `record_send`'s retention keeps, at most an hour of sends at 60 a minute, so it is
    /// scanned rather than indexed.
    ///
    /// # Errors
    ///
    /// When `after` is before the Unix epoch or the read fails.
    pub fn sends_after(&self, after: SystemTime) -> Result<u64, DatabaseError> {
        self.first(
            "SELECT count(*) FROM send WHERE sent_ns > ?1",
            [epoch_nanos(after).map_err(|error| failure(&self.path, error))?],
        )
        .map(Option::unwrap_or_default)
        .map_err(|error| failure(&self.path, error))
    }

    /// The pause flag.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn paused(&self) -> Result<bool, DatabaseError> {
        Ok(self.state(PAUSED)?.unwrap_or(false))
    }

    /// The first column of the first row `sql` returns, with the statement cached.
    fn first<T: rusqlite::types::FromSql>(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
    ) -> rusqlite::Result<Option<T>> {
        self.connection
            .prepare_cached(sql)?
            .query_row(params, |row| row.get(0))
            .optional()
    }

    fn failed_releases(&self) -> Result<Vec<(u64, String)>, DatabaseError> {
        let fail = |error| failure(&self.path, error);
        let mut statement = self
            .connection
            .prepare(
                "SELECT release_id, error FROM result WHERE error IS NOT NULL
                 ORDER BY fetched_at DESC, release_id LIMIT ?1",
            )
            .map_err(fail)?;
        statement
            .query_map([FAILED_SHOWN], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(fail)?
            .collect::<rusqlite::Result<_>>()
            .map_err(fail)
    }

    fn state<T: rusqlite::types::FromSql>(&self, key: &str) -> Result<Option<T>, DatabaseError> {
        self.first("SELECT value FROM state WHERE key = ?1", [key])
            .map_err(|error| failure(&self.path, error))
    }

    /// The counts and state for the status at time `now`, read in one transaction so they
    /// agree with each other while a sync writes. Whether a sync is running is not in the
    /// database: the caller reads the lock, before or after, and the two can differ by a
    /// sync that starts or ends in between.
    ///
    /// # Errors
    ///
    /// When a read fails, or a queued release has a source this build doesn't know.
    pub fn progress(&self, now: i64) -> Result<Progress, DatabaseError> {
        self.read(|| self.progress_now(now))
    }

    /// `progress` and the number of sends after `sends_after`, read in the same
    /// transaction, so the requests agree with the counts.
    ///
    /// # Errors
    ///
    /// As `progress`, or when `sends_after` is before the Unix epoch.
    pub fn progress_and_sends(
        &self,
        now: i64,
        sends_after: SystemTime,
    ) -> Result<(Progress, u64), DatabaseError> {
        self.read(|| Ok((self.progress_now(now)?, self.sends_after(sends_after)?)))
    }

    /// Runs `reads` in one read transaction, so every read in it sees the same snapshot.
    fn read<T>(
        &self,
        reads: impl FnOnce() -> Result<T, DatabaseError>,
    ) -> Result<T, DatabaseError> {
        let fail = |error| failure(&self.path, error);
        let transaction = self.connection.unchecked_transaction().map_err(fail)?;
        let read = reads()?;
        transaction.commit().map_err(fail)?;
        Ok(read)
    }

    /// The reads of `progress`, for the caller's transaction.
    fn progress_now(&self, now: i64) -> Result<Progress, DatabaseError> {
        let last_error = match (self.state(LAST_ERROR)?, self.state(LAST_ERROR_AT)?) {
            (Some(message), Some(at)) => Some((at, message)),
            _ => None,
        };
        let (library, candidates) = self
            .counts(now)
            .map_err(|error| failure(&self.path, error))?;
        Ok(Progress {
            library,
            candidates,
            paused: self.paused()?,
            last_error,
            yielding_since: self.state(YIELDING_SINCE)?,
            failures: self.failed_releases()?,
        })
    }

    /// The library's and the candidates' counts at time `now`.
    fn counts(&self, now: i64) -> rusqlite::Result<(Counts, Counts)> {
        let (mut library, mut candidates) = (Counts::default(), Counts::default());
        let mut statement = self.connection.prepare_cached(
            "SELECT q.source,
                    coalesce(sum(q.done = 0), 0),
                    coalesce(sum(r.rating_average IS NOT NULL), 0),
                    coalesce(sum(r.error IS NOT NULL), 0),
                    coalesce(sum(r.gone = 1), 0),
                    coalesce(sum(r.gone = 0 AND r.fetched_at <= ?1), 0)
             FROM queue q LEFT JOIN result r ON r.release_id = q.release_id
             GROUP BY q.source",
        )?;
        let mut rows = statement.query([refresh_cutoff(now)])?;
        while let Some(row) = rows.next()? {
            let source: i64 = row.get(0)?;
            let counts = match source {
                s if s == Source::Library as i64 => &mut library,
                s if s == Source::Candidate as i64 => &mut candidates,
                _ => return Err(rusqlite::Error::IntegralValueOutOfRange(0, source)),
            };
            *counts = Counts {
                queued: row.get(1)?,
                with_values: row.get(2)?,
                failed: row.get(3)?,
                gone: row.get(4)?,
                due: row.get(5)?,
            };
        }
        Ok((library, candidates))
    }
}

/// A queued release's row in `stored`'s query: `result`'s columns, NULL when it has none.
fn stored_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Stored> {
    let Some(at) = row.get::<_, Option<i64>>(0)? else {
        return Ok(Stored::Queued);
    };
    if row.get(1)? {
        return Ok(Stored::Gone { at });
    }
    // The four values scoring reads are written together, so `have` tells whether there
    // are any; a NULL among the other three is an error, not a missing value.
    let values = match row.get::<_, Option<u64>>(5)? {
        None => None,
        Some(have) => Some(Values {
            rating_average: row.get(3)?,
            rating_count: row.get(4)?,
            have,
            want: row.get(6)?,
            num_for_sale: row.get(7)?,
            lowest_price: row.get(8)?,
        }),
    };
    if row.get(2)? {
        return Ok(Stored::Failed { values, at });
    }
    match values {
        Some(values) => Ok(Stored::Fetched { values, at }),
        None => Err(rusqlite::Error::InvalidColumnType(
            5,
            row.as_ref().column_name(5)?.to_string(),
            rusqlite::types::Type::Null,
        )),
    }
}

/// Takes the release out of the never-fetched queue.
fn mark_done(connection: &Connection, release_id: u64) -> rusqlite::Result<()> {
    connection
        .execute(
            "UPDATE queue SET done = 1 WHERE release_id = ?1",
            [release_id],
        )
        .map(drop)
}

/// The last error for a problem with a release: the text `sync status` shows and the log
/// repeats.
fn release_error(release_id: u64, problem: &str) -> String {
    format!("release {release_id}: {problem}")
}

/// Replaces the last error and its time.
fn set_last_error(connection: &Connection, message: &str, now: i64) -> rusqlite::Result<()> {
    connection.execute(SET_STATE, params![LAST_ERROR, message])?;
    connection.execute(SET_STATE, params![LAST_ERROR_AT, now])?;
    Ok(())
}

/// Sets a release's `result` row to failed, keeping any reply and values, clears its retry
/// row and takes it out of the never-fetched queue.
fn mark_failed(
    connection: &Connection,
    release_id: u64,
    problem: &str,
    now: i64,
) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO result (release_id, fetched_at, gone, error) VALUES (?1, ?2, 0, ?3)
         ON CONFLICT (release_id) DO UPDATE SET
             fetched_at = excluded.fetched_at, error = excluded.error",
        params![release_id, now, problem],
    )?;
    connection.execute("DELETE FROM retry WHERE release_id = ?1", [release_id])?;
    mark_done(connection, release_id)
}

const SET_STATE: &str = "INSERT OR REPLACE INTO state (key, value) VALUES (?1, ?2)";

/// A time in nanoseconds since the Unix epoch, as the `send` table holds it.
fn epoch_nanos(at: SystemTime) -> rusqlite::Result<i64> {
    at.duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))
        .and_then(nanos)
}

/// A span in nanoseconds, as the `send` table holds it. Spans up to the year 2262 fit.
fn nanos(span: Duration) -> rusqlite::Result<i64> {
    i64::try_from(span.as_nanos())
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))
}

fn failure(path: &Path, error: rusqlite::Error) -> DatabaseError {
    DatabaseError::new(NAME, path, error)
}

/// How long a release waits after its `attempts`th release problem. `attempts` is below
/// `MAX_ATTEMPTS`, so the shift can't overflow.
fn retry_wait(attempts: u64) -> Duration {
    (RETRY_BASE * (1 << (attempts - 1))).min(RETRY_CAP)
}

/// Values fetched at or before this time are due at `now`.
fn refresh_cutoff(now: i64) -> i64 {
    now - REFRESH_AFTER.as_secs().cast_signed()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{Community, FILE_NAME, NEVER_FETCHED, Reading, Values};

    /// A fresh directory that is removed when it drops, so a failed assertion leaves none.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(test: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("catalog-{test}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_file_without_tables_reads_as_missing() {
        // A 0-byte file, and a SQLite database that once had a table and has none.
        for name in ["zero-bytes", "no-tables"] {
            let dir = TempDir::new(name);
            let path = dir.0.join(FILE_NAME);
            if name == "zero-bytes" {
                std::fs::write(&path, b"").unwrap();
            } else {
                rusqlite::Connection::open(&path)
                    .unwrap()
                    .execute_batch("CREATE TABLE t (x); DROP TABLE t;")
                    .unwrap();
            }
            let before = std::fs::read(&path).unwrap();
            assert!(
                matches!(Community::open_read_only(&dir.0), Ok(Reading::Missing)),
                "{name}"
            );
            assert_eq!(std::fs::read(&path).unwrap(), before, "{name}");
        }
    }

    /// The plan SQLite chooses for `sql`, one line per step.
    fn plan(db: &Community, sql: &str) -> String {
        let mut statement = db
            .connection
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .unwrap();
        statement
            .query_map([], |row| row.get::<_, String>(3))
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_read_sees_one_snapshot_while_another_connection_writes() {
        let dir = TempDir::new("snapshot");
        let mut writer = Community::open(&dir.0).unwrap();
        let reader = Community::open(&dir.0).unwrap();
        let never = std::time::SystemTime::UNIX_EPOCH;
        let sent = never + std::time::Duration::from_secs(100);
        let (first, second) = reader
            .read(|| {
                let first = reader.sends_after(never)?;
                writer.record_send(sent, std::time::Duration::from_hours(1))?;
                Ok((first, reader.sends_after(never)?))
            })
            .unwrap();
        assert_eq!((first, second), (0, 0));
        assert_eq!(reader.sends_after(never).unwrap(), 1);
    }

    #[test]
    fn syncing_is_true_while_a_sync_lock_is_held_and_not_after() {
        let dir = TempDir::new("syncing");
        assert!(!Community::syncing(&dir.0).unwrap());
        assert!(
            !dir.0.join(super::SYNC_RUNNING_FILE).exists(),
            "a check creates no file"
        );
        let lock = Community::lock(&dir.0).unwrap();
        assert!(Community::syncing(&dir.0).unwrap());
        // The check let go: it didn't take the sync's place.
        assert!(Community::syncing(&dir.0).unwrap());
        drop(lock);
        assert!(!Community::syncing(&dir.0).unwrap());
    }

    #[test]
    fn a_second_sync_lock_is_busy_at_once() {
        let dir = TempDir::new("sync-lock-busy");
        let lock = Community::lock(&dir.0).unwrap();
        let error = Community::lock(&dir.0).err().unwrap();
        assert!(matches!(error, super::LockError::Busy(_)), "{error:?}");
        drop(lock);
        Community::lock(&dir.0).unwrap();
    }

    #[test]
    fn two_checks_at_once_both_find_no_sync_running() {
        let dir = TempDir::new("syncing-twice");
        // One check's probe, held while another check runs.
        let probe = std::fs::File::create(dir.0.join(super::SYNC_RUNNING_FILE)).unwrap();
        probe.lock_shared().unwrap();
        assert!(!Community::syncing(&dir.0).unwrap());
    }

    #[test]
    fn a_check_in_progress_doesnt_make_the_sync_lock_busy() {
        let dir = TempDir::new("sync-lock-probe");
        // A check holding its shared probe on `sync.running`.
        let probe = std::fs::File::create(dir.0.join(super::SYNC_RUNNING_FILE)).unwrap();
        probe.lock_shared().unwrap();
        let (about_to_lock, wait_for_thread) = std::sync::mpsc::channel();
        let path = dir.0.clone();
        let sync = std::thread::spawn(move || {
            about_to_lock.send(()).unwrap();
            Community::lock(&path)
        });
        // The thread signals just before `lock`, and the probe is released after that.
        // This can't prove `lock` was already waiting when the probe went: a `lock` that
        // doesn't wait fails only if the thread reaches it before the release, so a
        // regression to a non-blocking lock is caught only when the thread wins that race.
        wait_for_thread.recv().unwrap();
        drop(probe);
        let lock = sync.join().unwrap();
        assert!(lock.is_ok(), "{:?}", lock.err());
    }

    #[test]
    fn open_gives_up_on_a_lock_another_process_holds_and_names_the_lock_file() {
        let dir = TempDir::new("open-lock");
        let held = std::fs::File::create(dir.0.join(super::OPEN_LOCK_FILE)).unwrap();
        held.lock().unwrap();
        let error = Community::open_waiting(&dir.0, std::time::Duration::ZERO)
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("community.open.lock"), "{error}");
        drop(held);
        Community::open_waiting(&dir.0, std::time::Duration::ZERO).unwrap();
    }

    #[test]
    fn a_failed_release_keeps_the_values_of_its_last_fetch() {
        let dir = TempDir::new("keep");
        let mut db = Community::open(&dir.0).unwrap();
        db.queue(&[7], super::Source::Library).unwrap();
        let values = Values {
            rating_average: 4.25,
            rating_count: 31,
            have: 1200,
            want: 640,
            num_for_sale: None,
            lowest_price: None,
        };
        db.record(7, values, "{}", 100).unwrap();
        for at in 200..210 {
            db.record_failure(7, "Discogs answered 503", at).unwrap();
        }
        let (error, have, at): (String, i64, i64) = db
            .connection
            .query_row(
                "SELECT error, have, fetched_at FROM result WHERE release_id = 7",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            (error.as_str(), have, at),
            ("Discogs answered 503", 1200, 209)
        );
    }

    #[test]
    fn next_due_reads_the_queue_in_index_order_without_sorting() {
        let dir = TempDir::new("plan");
        let db = Community::open(&dir.0).unwrap();
        let never = plan(&db, &NEVER_FETCHED.replace("?1", "5"));
        assert!(
            never.contains("SCAN q USING INDEX queue_todo")
                || never.contains("SCAN q USING COVERING INDEX queue_todo"),
            "{never}"
        );
        assert!(!never.contains("TEMP B-TREE"), "{never}");
        let refresh = plan(&db, &super::REFRESH.replace("?1", "5").replace("?2", "6"));
        assert!(
            refresh.contains("COVERING INDEX result_fetched_at"),
            "{refresh}"
        );
        assert!(!refresh.contains("TEMP B-TREE"), "{refresh}");
    }

    #[test]
    fn the_reply_is_in_its_own_table_and_a_404_drops_it() {
        let dir = TempDir::new("reply-table");
        let mut db = Community::open(&dir.0).unwrap();
        db.queue(&[7], super::Source::Library).unwrap();
        let values = Values {
            rating_average: 4.0,
            rating_count: 1,
            have: 1,
            want: 1,
            num_for_sale: None,
            lowest_price: None,
        };
        db.record(7, values, "{\"id\": 7}", 100).unwrap();
        let stored: String = db
            .connection
            .query_row(
                "SELECT reply FROM result_reply WHERE release_id = 7",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, "{\"id\": 7}");
        db.record_gone(7, 200).unwrap();
        let rows: i64 = db
            .connection
            .query_row("SELECT count(*) FROM result_reply", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[test]
    fn records_the_values_under_their_release_id() {
        let dir = TempDir::new("record");
        let mut db = Community::open(&dir.0).unwrap();
        db.queue(&[7], super::Source::Library).unwrap();
        let values = Values {
            rating_average: 4.25,
            rating_count: 31,
            have: 1200,
            want: 640,
            num_for_sale: Some(15),
            lowest_price: Some(8.5),
        };
        db.record(7, values, "{}", 100).unwrap();
        let stored = db
            .connection
            .query_row(
                "SELECT rating_average, rating_count, have, want, num_for_sale, lowest_price,
                        fetched_at FROM result WHERE release_id = 7",
                [],
                |row| {
                    Ok((
                        Values {
                            rating_average: row.get(0)?,
                            rating_count: row.get(1)?,
                            have: row.get(2)?,
                            want: row.get(3)?,
                            num_for_sale: row.get(4)?,
                            lowest_price: row.get(5)?,
                        },
                        row.get::<_, i64>(6)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(stored, (values, 100));
    }
}
