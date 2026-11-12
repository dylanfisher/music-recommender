//! The community database: the sync's queue of Discogs API requests (a release, an artist's
//! releases, a master's versions), the replies fetched for them with the values scoring
//! reads, the rows of the list replies, and the sync's state (the pause flag, the last
//! error, when it started yielding to another client, its recent sends). Its own file,
//! which the catalog's monthly rebuild never replaces (ADR 0004, ADR 0030).
//!
//! Every time is passed in, in seconds since the Unix epoch, so callers and tests choose
//! the clock. Send times are the exception: `record_send` takes a `SystemTime`, kept to the
//! nanosecond.

use std::fmt;
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

/// `request` holds every request ever queued, each once by its kind, id and page, with its
/// tier and its priority within the tier; `seq` is the order it was first queued in. `result`
/// is one row per request fetched, replaced on each fetch: for a release the values read
/// from the reply, or a gone flag; a list's values are its rows, in `listed`. `result_reply`
/// has the reply's JSON as Discogs sent it, about 15 KB for a release and 60 KB for a list
/// page, in a table of its own so that scans of `result` (the counts in `progress`) don't
/// read it. A request is never fetched while it has no `result` row: `request.done` is 1
/// exactly when it has one, set in the same transaction as the row, so the never-fetched
/// query reads only the requests left. A failed row keeps the values and reply of the last
/// fetch, if any, and adds the last error, which is what marks it failed. `retry` has a row
/// for a request with problems since its last fetch: how many and when it may be tried
/// again. `listed` has each release or master a list reply named, by its kind (`ListedKind`)
/// and id, with its owners and wants and when it was listed; a release listed among a
/// master's versions has that master. `state` has no column type, so integers stay integers.
/// `send` has a row for each request the sync sent within the retention `record_send` was
/// given of its last, written before the request goes out: its time in nanoseconds since the
/// Unix epoch, since the pacer compares sends a fraction of a second apart.
///
/// `request_todo` holds only the requests not done, in the order `next_due` wants them, so a
/// request reads the first one without sorting or passing over those already fetched.
/// `listed_master` serves a master's most-owned version.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS request (
    seq INTEGER PRIMARY KEY,
    kind INTEGER NOT NULL,
    id INTEGER NOT NULL,
    page INTEGER NOT NULL,
    tier INTEGER NOT NULL,
    priority REAL NOT NULL,
    done INTEGER NOT NULL DEFAULT 0,
    UNIQUE (kind, id, page)
);
CREATE INDEX IF NOT EXISTS request_todo ON request (tier, priority DESC, seq) WHERE done = 0;
CREATE TABLE IF NOT EXISTS result (
    request INTEGER PRIMARY KEY REFERENCES request (seq),
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
    request INTEGER PRIMARY KEY REFERENCES request (seq),
    reply TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS retry (
    request INTEGER PRIMARY KEY REFERENCES request (seq),
    attempts INTEGER NOT NULL,
    retry_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS listed (
    kind INTEGER NOT NULL,
    id INTEGER NOT NULL,
    master_id INTEGER,
    have INTEGER NOT NULL,
    want INTEGER NOT NULL,
    listed_at INTEGER NOT NULL,
    PRIMARY KEY (kind, id)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS listed_master ON listed (master_id, have) WHERE master_id IS NOT NULL;
CREATE TABLE IF NOT EXISTS state (
    key TEXT PRIMARY KEY,
    value NOT NULL
);
CREATE TABLE IF NOT EXISTS send (
    sent_ns INTEGER NOT NULL
);
";

/// The first request never fetched whose retry time, if it has one, has passed: the most
/// urgent tier first, the highest priority in it, then the first queued. `?1` is the time
/// now.
const NEVER_FETCHED: &str = "SELECT q.kind, q.id, q.page FROM request q WHERE q.done = 0
     AND NOT EXISTS (SELECT 1 FROM retry t WHERE t.request = q.seq AND t.retry_at > ?1)
     ORDER BY q.tier, q.priority DESC, q.seq LIMIT 1";

/// The request with the oldest reply among those due for a refresh whose retry time, if it
/// has one, has passed. `?1` is the cutoff for the reply's age, `?2` the time now.
const REFRESH: &str =
    "SELECT q.kind, q.id, q.page FROM result r JOIN request q ON q.seq = r.request
     WHERE r.gone = 0 AND r.fetched_at <= ?1
     AND NOT EXISTS (SELECT 1 FROM retry t WHERE t.request = r.request AND t.retry_at > ?2)
     ORDER BY r.fetched_at, r.request LIMIT 1";

/// A request's `seq`, by its kind, id and page.
const SEQ: &str = "SELECT seq FROM request WHERE kind = ?1 AND id = ?2 AND page = ?3";

const PAUSED: &str = "paused";
/// How many failed releases `progress` lists.
const FAILED_SHOWN: usize = 10;

const LAST_ERROR: &str = "last_error";
const LAST_ERROR_AT: &str = "last_error_at";
const YIELDING_SINCE: &str = "yielding_since";
const SWEPT: &str = "swept";

/// What a request asks the API for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RequestKind {
    /// `GET /releases/{id}`: one release, with its rating.
    Release = 0,
    /// `GET /artists/{id}/releases`: a page of an artist's releases and masters, each with
    /// its owners and wants.
    ArtistReleases = 1,
    /// `GET /masters/{id}/versions`: a page of a master's releases, each with its owners and
    /// wants.
    MasterVersions = 2,
}

impl RequestKind {
    fn read(value: i64) -> rusqlite::Result<Self> {
        match value {
            0 => Ok(Self::Release),
            1 => Ok(Self::ArtistReleases),
            2 => Ok(Self::MasterVersions),
            _ => Err(rusqlite::Error::IntegralValueOutOfRange(0, value)),
        }
    }
}

/// One request: its kind, the Discogs id it is about and, for a list, its page from 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Request {
    /// What it asks for.
    pub kind: RequestKind,
    /// The release's, artist's or master's id.
    pub id: u64,
    /// The page, from 1; 1 for a release.
    pub page: u64,
}

impl Request {
    /// The request for release `id`.
    #[must_use]
    pub const fn release(id: u64) -> Self {
        Self {
            kind: RequestKind::Release,
            id,
            page: 1,
        }
    }

    /// The same list's next page.
    #[must_use]
    pub const fn next_page(self) -> Self {
        Self {
            page: self.page + 1,
            ..self
        }
    }

    fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            kind: RequestKind::read(row.get(0)?)?,
            id: row.get(1)?,
            page: row.get(2)?,
        })
    }
}

/// How the log, the last error and `sync status` name a request: `release 7`, `artist 7's
/// releases, page 2`, `master 7's versions, page 1`.
impl fmt::Display for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { id, page, .. } = self;
        match self.kind {
            RequestKind::Release => write!(f, "release {id}"),
            RequestKind::ArtistReleases => write!(f, "artist {id}'s releases, page {page}"),
            RequestKind::MasterVersions => write!(f, "master {id}'s versions, page {page}"),
        }
    }
}

/// Which part of the queue a request is in, the most urgent first (ADR 0030). Within a tier
/// the highest priority goes first, then the first queued.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    /// A release the owner has, from the profile.
    Library = 0,
    /// A representative of a list the owner asked for; its priority is when the list was
    /// last asked for.
    Asked = 1,
    /// The popularity sweep's artists' releases.
    Artists = 2,
    /// The popularity sweep's masters' versions.
    Masters = 3,
    /// A candidate's representative, by the scorer's priority.
    Ratings = 4,
}

/// What a row of a list reply is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ListedKind {
    /// A release.
    Release = 0,
    /// A master, as an artist's releases list it.
    Master = 1,
}

/// One row of a list reply: a release or master, with its owners and wants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Listed {
    /// A release or a master.
    pub kind: ListedKind,
    /// Its Discogs id.
    pub id: u64,
    /// `stats.community.in_collection`.
    pub have: u64,
    /// `stats.community.in_wantlist`.
    pub want: u64,
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

/// One part's requests, as `progress` counts them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// Queued and never fetched.
    pub queued: u64,
    /// With values from a fetch, including failed releases that had some; for lists,
    /// fetched and not failed.
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
    /// Every other release: the asked lists', the candidates'.
    pub candidates: Counts,
    /// The popularity sweep's list requests, every page counted.
    pub lists: Counts,
    /// The pause flag.
    pub paused: bool,
    /// The last error recorded and its time, if any.
    pub last_error: Option<(i64, String)>,
    /// When the running sync started yielding to another client of the Discogs API (the
    /// player's tagger), if it is yielding. A sync killed while yielding leaves it set, so
    /// it means something only while a sync holds the lock.
    pub yielding_since: Option<i64>,
    /// The `FAILED_SHOWN` most recent failed releases, newest first, with their last error.
    /// A failed list request is counted in `lists`, not shown.
    pub failures: Vec<(u64, String)>,
}

impl Progress {
    /// The library's, the candidates' and the lists' counts added together.
    #[must_use]
    pub fn total(&self) -> Counts {
        let (a, b, c) = (self.library, self.candidates, self.lists);
        Counts {
            queued: a.queued + b.queued + c.queued,
            with_values: a.with_values + b.with_values + c.with_values,
            failed: a.failed + b.failed + c.failed,
            gone: a.gone + b.gone + c.gone,
            due: a.due + b.due + c.due,
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
    hold_running(lock_waiting(lock, waiting)?, running)
}

/// An exclusive lock on `lock` that waits its turn instead of being refused, with no running
/// file: while another process holds it, `waiting` is called once and the lock waited for.
pub(crate) fn lock_waiting(lock: PathBuf, waiting: impl FnOnce()) -> Result<SyncLock, LockError> {
    match try_lock(lock) {
        Err(LockError::Busy(path)) => {
            waiting();
            let file = open_lock_file(&path)?;
            file.lock().map_err(|error| LockError::Io(path, error))?;
            Ok(SyncLock {
                _file: file,
                _running: None,
            })
        }
        taken => taken,
    }
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

    /// Adds requests to the queue in `tier`, each with its priority, in order, and returns
    /// how many weren't queued before. A request queued before in this tier or a less urgent
    /// one takes this tier and priority, so the scorer rewrites priorities and a release
    /// queued as a candidate and now in the library moves up to the library's; one in a more
    /// urgent tier stays there. Either way it keeps its place among equal priorities, and a
    /// request fetched isn't fetched again before its refresh.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn queue(&mut self, requests: &[(Request, f64)], tier: Tier) -> Result<u64, DatabaseError> {
        let fail = |error| failure(&self.path, error);
        let tier = tier as i64;
        let transaction = self.connection.transaction().map_err(fail)?;
        let mut added = 0;
        {
            let mut insert = transaction
                .prepare(
                    "INSERT OR IGNORE INTO request (kind, id, page, tier, priority)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                )
                .map_err(fail)?;
            let mut update = transaction
                .prepare(
                    "UPDATE request SET tier = ?4, priority = ?5
                     WHERE kind = ?1 AND id = ?2 AND page = ?3 AND tier >= ?4",
                )
                .map_err(fail)?;
            for &(request, priority) in requests {
                let values = params![
                    request.kind as i64,
                    request.id,
                    request.page,
                    tier,
                    priority
                ];
                if insert.execute(values).map_err(fail)? == 0 {
                    update.execute(values).map_err(fail)?;
                } else {
                    added += 1;
                }
            }
        }
        transaction.commit().map_err(fail)?;
        Ok(added)
    }

    /// `queue` for release ids, all at one priority, as the profile queues the library's.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn queue_releases(
        &mut self,
        release_ids: &[u64],
        tier: Tier,
    ) -> Result<u64, DatabaseError> {
        let requests: Vec<(Request, f64)> = release_ids
            .iter()
            .map(|&id| (Request::release(id), 0.0))
            .collect();
        self.queue(&requests, tier)
    }

    /// The request to send next at time `now`: the first never fetched, by tier, then
    /// priority, then the order queued; then the one whose reply is oldest, if it is
    /// `REFRESH_AFTER` old. A request with problems on record is skipped until its retry
    /// time. `None` when nothing is due.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn next_due(&self, now: i64) -> Result<Option<Request>, DatabaseError> {
        let fail = |error| failure(&self.path, error);
        let read = |sql: &str, params: &[&dyn rusqlite::ToSql]| {
            self.connection
                .prepare_cached(sql)?
                .query_row(params, Request::read)
                .optional()
        };
        let never = read(NEVER_FETCHED, &[&now]).map_err(fail)?;
        if never.is_some() {
            return Ok(never);
        }
        read(REFRESH, &[&refresh_cutoff(now), &now]).map_err(fail)
    }

    /// Records the reply fetched for release `release_id` at time `now`, its JSON as Discogs
    /// sent it and the values read from it, replacing older ones.
    ///
    /// # Errors
    ///
    /// When the release isn't queued or the write fails.
    pub fn record(
        &mut self,
        release_id: u64,
        values: Values,
        reply: &str,
        now: i64,
    ) -> Result<(), DatabaseError> {
        self.replace_result(
            Request::release(release_id),
            Some(reply),
            "INSERT OR REPLACE INTO result
             (request, fetched_at, gone, rating_average, rating_count, have, want,
              num_for_sale, lowest_price)
             VALUES (?1, ?2, 0, ?3, ?4, ?5, ?6, ?7, ?8)",
            &[
                &now,
                &values.rating_average,
                &values.rating_count,
                &values.have,
                &values.want,
                &values.num_for_sale,
                &values.lowest_price,
            ],
        )
    }

    /// Records the reply fetched for list request `request` at time `now`, its JSON as
    /// Discogs sent it, and its rows in `listed`, replacing what they had; a release listed
    /// among a master's versions keeps that master. When `next` is given, that page is
    /// queued in the tier and at the priority of this one, in the same transaction, so a
    /// list's next page is queued only once its page before is in.
    ///
    /// # Errors
    ///
    /// When the request isn't queued or the write fails.
    pub fn record_list(
        &mut self,
        request: Request,
        rows: &[Listed],
        reply: &str,
        next: Option<Request>,
        now: i64,
    ) -> Result<(), DatabaseError> {
        let master = (request.kind == RequestKind::MasterVersions).then_some(request.id);
        let fail = |error| failure(&self.path, error);
        let seq = self.seq(request)?;
        let transaction = self.connection.transaction().map_err(fail)?;
        {
            let mut insert = transaction
                .prepare_cached(
                    "INSERT OR REPLACE INTO listed (kind, id, master_id, have, want, listed_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                )
                .map_err(fail)?;
            for row in rows {
                insert
                    .execute(params![
                        row.kind as i64,
                        row.id,
                        master,
                        row.have,
                        row.want,
                        now
                    ])
                    .map_err(fail)?;
            }
            if let Some(next) = next {
                transaction
                    .execute(
                        "INSERT OR IGNORE INTO request (kind, id, page, tier, priority)
                         SELECT ?2, ?3, ?4, tier, priority FROM request WHERE seq = ?1",
                        params![seq, next.kind as i64, next.id, next.page],
                    )
                    .map_err(fail)?;
            }
        }
        write_result(
            &transaction,
            seq,
            Some(reply),
            "INSERT OR REPLACE INTO result (request, fetched_at, gone) VALUES (?1, ?2, 0)",
            &[&now],
        )
        .map_err(fail)?;
        transaction.commit().map_err(fail)
    }

    /// The reply last stored for a request, as Discogs sent it. `None` when it has none:
    /// never fetched, or gone.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn reply(&self, request: Request) -> Result<Option<String>, DatabaseError> {
        self.first(
            "SELECT p.reply FROM request q JOIN result_reply p ON p.request = q.seq
             WHERE q.kind = ?1 AND q.id = ?2 AND q.page = ?3",
            params![request.kind as i64, request.id, request.page],
        )
        .map_err(|error| failure(&self.path, error))
    }

    /// The owners a list reply gave for a release or master, `None` when none listed it.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn listed(&self, kind: ListedKind, id: u64) -> Result<Option<u64>, DatabaseError> {
        self.first(
            "SELECT have FROM listed WHERE kind = ?1 AND id = ?2",
            params![kind as i64, id],
        )
        .map_err(|error| failure(&self.path, error))
    }

    /// The release a master's versions list as the most owned, the lowest id among equals;
    /// `None` when no versions of it were listed.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn most_owned(&self, master_id: u64) -> Result<Option<u64>, DatabaseError> {
        self.first(
            "SELECT id FROM listed WHERE master_id = ?1 ORDER BY have DESC, id LIMIT 1",
            [master_id],
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
                     FROM request q LEFT JOIN result r ON r.request = q.seq
                     WHERE q.kind = 0 AND q.id = ?1 AND q.page = 1",
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

    /// Records that the API answered 404 for a request at time `now`. Its old reply and
    /// values, if any, are dropped; a list's rows stay, since other lists may name them.
    ///
    /// # Errors
    ///
    /// When the request isn't queued or the write fails.
    pub fn record_gone(&mut self, request: Request, now: i64) -> Result<(), DatabaseError> {
        self.replace_result(
            request,
            None,
            "INSERT OR REPLACE INTO result (request, fetched_at, gone) VALUES (?1, ?2, 1)",
            &[&now],
        )
    }

    /// Counts a problem with a request's reply and sets the last error naming the request,
    /// in one transaction. The request waits to be tried again: a minute after the first,
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
        request: Request,
        problem: &str,
        now: i64,
    ) -> Result<(Failure, String), DatabaseError> {
        let fail = |error| failure(&self.path, error);
        let seq = self.seq(request)?;
        let transaction = self.connection.transaction().map_err(fail)?;
        let attempts: u64 = transaction
            .prepare_cached("SELECT attempts FROM retry WHERE request = ?1")
            .and_then(|mut statement| statement.query_row([seq], |row| row.get(0)).optional())
            .map_err(fail)?
            .unwrap_or(0)
            + 1;
        let outcome = if attempts >= MAX_ATTEMPTS {
            mark_failed(&transaction, seq, problem, now).map_err(fail)?;
            Failure::Failed
        } else {
            transaction
                .prepare_cached(
                    "INSERT OR REPLACE INTO retry (request, attempts, retry_at)
                     VALUES (?1, ?2, ?3)",
                )
                .and_then(|mut statement| {
                    statement.execute(params![
                        seq,
                        attempts,
                        now + retry_wait(attempts).as_secs().cast_signed()
                    ])
                })
                .map_err(fail)?;
            Failure::Retry
        };
        let message = request_error(request, problem);
        set_last_error(&transaction, &message, now).map_err(fail)?;
        transaction.commit().map_err(fail)?;
        Ok((outcome, message))
    }

    /// Sets the last error naming the request, for a reply that leaves the request as it
    /// was (a 429, trouble with Discogs or the network). Returns its text, for the log.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn record_request_error(
        &mut self,
        request: Request,
        problem: &str,
        now: i64,
    ) -> Result<String, DatabaseError> {
        let message = request_error(request, problem);
        self.record_error(&message, now)?;
        Ok(message)
    }

    /// Replaces a request's `result` row with `sql`, whose `?1` is the request's `seq` and
    /// the rest `values`, and its reply, or drops the reply when `reply` is `None`; clears
    /// its retry row and marks it done, together.
    fn replace_result(
        &mut self,
        request: Request,
        reply: Option<&str>,
        sql: &str,
        values: &[&dyn rusqlite::ToSql],
    ) -> Result<(), DatabaseError> {
        let fail = |error| failure(&self.path, error);
        let seq = self.seq(request)?;
        let transaction = self.connection.transaction().map_err(fail)?;
        write_result(&transaction, seq, reply, sql, values).map_err(fail)?;
        transaction.commit().map_err(fail)
    }

    /// The `seq` of a queued request.
    ///
    /// # Errors
    ///
    /// When the read fails or the request was never queued: a reply is only recorded for a
    /// request the queue gave.
    fn seq(&self, request: Request) -> Result<i64, DatabaseError> {
        self.first(SEQ, params![request.kind as i64, request.id, request.page])
            .map_err(|error| failure(&self.path, error))?
            .ok_or_else(|| {
                DatabaseError::new(
                    NAME,
                    &self.path,
                    std::io::Error::other(format!("{request} was never queued")),
                )
            })
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

    /// When the list the popularity sweep was last queued for was computed, in seconds since
    /// the Unix epoch; `None` before the first sweep.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn swept(&self) -> Result<Option<i64>, DatabaseError> {
        self.state(SWEPT)
    }

    /// Records that the sweep was queued for the list computed at `computed_at`.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn set_swept(&self, computed_at: i64) -> Result<(), DatabaseError> {
        self.connection
            .execute(SET_STATE, params![SWEPT, computed_at])
            .map(drop)
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
                "SELECT q.id, r.error FROM result r JOIN request q ON q.seq = r.request
                 WHERE r.error IS NOT NULL AND q.kind = 0
                 ORDER BY r.fetched_at DESC, q.id LIMIT ?1",
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
        let (library, candidates, lists) = self
            .counts(now)
            .map_err(|error| failure(&self.path, error))?;
        Ok(Progress {
            library,
            candidates,
            lists,
            paused: self.paused()?,
            last_error,
            yielding_since: self.state(YIELDING_SINCE)?,
            failures: self.failed_releases()?,
        })
    }

    /// The library's, the other releases' and the lists' counts at time `now`.
    fn counts(&self, now: i64) -> rusqlite::Result<(Counts, Counts, Counts)> {
        let mut parts = [Counts::default(); 3];
        // The part: 0 the library's releases, 1 the other releases, 2 the lists. A list has
        // no values, so a list fetched and not failed counts as with values.
        let mut statement = self.connection.prepare_cached(
            "SELECT CASE WHEN q.kind != 0 THEN 2 WHEN q.tier = 0 THEN 0 ELSE 1 END AS part,
                    coalesce(sum(q.done = 0), 0),
                    coalesce(sum(CASE WHEN q.kind = 0 THEN r.rating_average IS NOT NULL
                                 ELSE r.gone = 0 AND r.error IS NULL END), 0),
                    coalesce(sum(r.error IS NOT NULL), 0),
                    coalesce(sum(r.gone = 1), 0),
                    coalesce(sum(r.gone = 0 AND r.fetched_at <= ?1), 0)
             FROM request q LEFT JOIN result r ON r.request = q.seq
             GROUP BY part",
        )?;
        let mut rows = statement.query([refresh_cutoff(now)])?;
        while let Some(row) = rows.next()? {
            let part: usize = row.get(0)?;
            parts[part] = Counts {
                queued: row.get(1)?,
                with_values: row.get(2)?,
                failed: row.get(3)?,
                gone: row.get(4)?,
                due: row.get(5)?,
            };
        }
        let [library, candidates, lists] = parts;
        Ok((library, candidates, lists))
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

/// Writes request `seq`'s `result` row with `sql`, whose `?1` is `seq` and the rest
/// `values`, and its reply, or drops the reply when `reply` is `None`; clears its retry row
/// and marks it done.
fn write_result(
    connection: &Connection,
    seq: i64,
    reply: Option<&str>,
    sql: &str,
    values: &[&dyn rusqlite::ToSql],
) -> rusqlite::Result<()> {
    let params: Vec<&dyn rusqlite::ToSql> = std::iter::once(&seq as &dyn rusqlite::ToSql)
        .chain(values.iter().copied())
        .collect();
    connection.execute(sql, params.as_slice())?;
    match reply {
        Some(reply) => connection.execute(
            "INSERT OR REPLACE INTO result_reply (request, reply) VALUES (?1, ?2)",
            params![seq, reply],
        ),
        None => connection.execute("DELETE FROM result_reply WHERE request = ?1", [seq]),
    }?;
    connection.execute("DELETE FROM retry WHERE request = ?1", [seq])?;
    mark_done(connection, seq)
}

/// Takes the request out of the never-fetched queue.
fn mark_done(connection: &Connection, seq: i64) -> rusqlite::Result<()> {
    connection
        .execute("UPDATE request SET done = 1 WHERE seq = ?1", [seq])
        .map(drop)
}

/// The last error for a problem with a request: the text `sync status` shows and the log
/// repeats.
fn request_error(request: Request, problem: &str) -> String {
    format!("{request}: {problem}")
}

/// Replaces the last error and its time.
fn set_last_error(connection: &Connection, message: &str, now: i64) -> rusqlite::Result<()> {
    connection.execute(SET_STATE, params![LAST_ERROR, message])?;
    connection.execute(SET_STATE, params![LAST_ERROR_AT, now])?;
    Ok(())
}

/// Sets a request's `result` row to failed, keeping any reply and values, clears its retry
/// row and takes it out of the never-fetched queue.
fn mark_failed(connection: &Connection, seq: i64, problem: &str, now: i64) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO result (request, fetched_at, gone, error) VALUES (?1, ?2, 0, ?3)
         ON CONFLICT (request) DO UPDATE SET
             fetched_at = excluded.fetched_at, error = excluded.error",
        params![seq, now, problem],
    )?;
    connection.execute("DELETE FROM retry WHERE request = ?1", [seq])?;
    mark_done(connection, seq)
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
        db.queue_releases(&[7], super::Tier::Library).unwrap();
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
            db.record_failure(super::Request::release(7), "Discogs answered 503", at)
                .unwrap();
        }
        let (error, have, at): (String, i64, i64) = db
            .connection
            .query_row(
                "SELECT error, have, fetched_at FROM result WHERE request = 1",
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
            never.contains("SCAN q USING INDEX request_todo")
                || never.contains("SCAN q USING COVERING INDEX request_todo"),
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
        db.queue_releases(&[7], super::Tier::Library).unwrap();
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
                "SELECT reply FROM result_reply WHERE request = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, "{\"id\": 7}");
        db.record_gone(super::Request::release(7), 200).unwrap();
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
        db.queue_releases(&[7], super::Tier::Library).unwrap();
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
                        fetched_at FROM result WHERE request = 1",
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
