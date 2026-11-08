//! The community database: the sync's queue of Discogs release ids, the ratings and
//! have/want counts fetched for them, and the sync's state (the pause flag, the last
//! error). Its own file, which the catalog's monthly rebuild never replaces (ADR 0004).
//!
//! Every time is passed in, in seconds since the Unix epoch, so callers and tests choose
//! the clock.

use std::fmt;
use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::{Connection, OptionalExtension, params};

/// The database's file name inside the data directory.
const FILE_NAME: &str = "community.sqlite";

/// How often `open` tries the lock file again while another process holds it.
const LOCK_RETRY_EVERY: Duration = Duration::from_millis(20);

/// The lock file `open` holds while it checks, deletes and creates the database file.
const OPEN_LOCK_FILE: &str = "community.open.lock";

/// A value this old is fetched again, behind every release never fetched.
const REFRESH_AFTER: Duration = Duration::from_hours(90 * 24);

/// How long a write waits for another process (a `sync pause` during a sync) to finish.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// A release is marked failed at its tenth release problem since its last fetch.
const MAX_ATTEMPTS: u64 = 10;

/// After its first release problem a release waits this long, doubling for each one after
/// it...
const RETRY_BASE: Duration = Duration::from_mins(1);

/// ...up to this long.
const RETRY_CAP: Duration = Duration::from_hours(24);

/// `queue` holds every release id ever queued, in order (`seq`, library first). `result` is
/// one row per release fetched, replaced on each fetch. A release is never fetched while it
/// has no `result` row: `queue.done` is 1 exactly when it has one, set in the same
/// transaction as the row, so the never-fetched query reads only the releases left. A
/// failed row keeps the values of the last fetch, if any, and adds the last error. `retry`
/// has a row for a release with problems since its last fetch: how many, when it may be
/// tried again and the last error. `state` has no column type, so integers stay integers.
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
    failed INTEGER NOT NULL DEFAULT 0,
    error TEXT,
    rating_average REAL,
    rating_count INTEGER,
    have INTEGER,
    want INTEGER
);
CREATE INDEX IF NOT EXISTS result_fetched_at ON result (fetched_at) WHERE gone = 0;
CREATE TABLE IF NOT EXISTS retry (
    release_id INTEGER PRIMARY KEY REFERENCES queue (release_id),
    attempts INTEGER NOT NULL,
    retry_at INTEGER NOT NULL,
    last_error TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS state (
    key TEXT PRIMARY KEY,
    value NOT NULL
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

/// Where a queued release id came from. The library's are fetched before candidates'.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// A release the owner has, from the profile.
    Library = 0,
    /// A release a recommendation list suggests.
    Candidate = 1,
}

/// The community values the API reports for a release, under the API's names.
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
}

/// What a release problem did to a release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The release will be tried again. Holds its problems so far.
    Retry(u64),
    /// That was the last allowed: the release is marked failed and comes due again at the
    /// refresh.
    Failed,
}

/// Counts and state for `sync status`.
#[derive(Debug, PartialEq, Eq)]
pub struct Progress {
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
    /// The pause flag.
    pub paused: bool,
    /// The last error recorded and its time, if any.
    pub last_error: Option<(i64, String)>,
    /// The `FAILED_SHOWN` most recent failed releases, newest first, with their last error.
    pub failures: Vec<(u64, String)>,
}

/// A failed read or write, naming the database file.
#[derive(Debug)]
pub struct CommunityError {
    path: PathBuf,
    error: Box<dyn std::error::Error + Send + Sync>,
}

impl fmt::Display for CommunityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "community database {}: {}",
            self.path.display(),
            self.error
        )
    }
}

impl std::error::Error for CommunityError {}

/// An open community database.
pub struct Community {
    path: PathBuf,
    connection: Connection,
}

impl Community {
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
    pub fn open(data_dir: &Path) -> Result<Self, CommunityError> {
        Self::open_waiting(data_dir, BUSY_TIMEOUT)
    }

    /// `open`, waiting at most `lock_timeout` for the open lock.
    fn open_waiting(data_dir: &Path, lock_timeout: Duration) -> Result<Self, CommunityError> {
        let path = data_dir.join(FILE_NAME);
        let fail = |error| failure(&path, error);
        let io_fail = |error: std::io::Error| CommunityError {
            path: path.clone(),
            error: Box::new(error),
        };
        if !data_dir.is_dir() {
            return Err(fail(rusqlite::Error::InvalidPath(data_dir.to_path_buf())));
        }
        let lock_path = data_dir.join(OPEN_LOCK_FILE);
        let lock = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .map_err(io_fail)?;
        let deadline = Instant::now() + lock_timeout;
        loop {
            match lock.try_lock() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(LOCK_RETRY_EVERY);
                }
                Err(TryLockError::WouldBlock) => {
                    return Err(io_fail(std::io::Error::new(
                        std::io::ErrorKind::WouldBlock,
                        format!(
                            "{} is held by another process; waited {}",
                            lock_path.display(),
                            if lock_timeout < Duration::from_secs(1) {
                                format!("{} ms", lock_timeout.as_millis())
                            } else {
                                format!("{} s", lock_timeout.as_secs())
                            }
                        ),
                    )));
                }
                Err(TryLockError::Error(error)) => return Err(io_fail(error)),
            }
        }
        let mut connection = connect(&path).map_err(fail)?;
        // Pre-release only (AGENTS.md): delete this check with the pre-release line there.
        if has_other_shape(&connection).map_err(fail)? {
            drop(connection);
            for suffix in ["", "-wal", "-shm"] {
                let mut file = path.clone().into_os_string();
                file.push(suffix);
                match std::fs::remove_file(&file) {
                    Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                        return Err(io_fail(error));
                    }
                    _ => {}
                }
            }
            eprintln!(
                "community database {}: its tables are from another version, so the file was \
                 deleted and started again; load the profile again",
                path.display()
            );
            connection = connect(&path).map_err(fail)?;
        }
        connection.execute_batch(SCHEMA).map_err(fail)?;
        drop(lock);
        Ok(Self { path, connection })
    }

    /// Adds release ids to the queue, in order, and returns how many weren't queued
    /// before. An id already queued keeps its place; one queued as a candidate and now
    /// in the library moves up to the library's.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn queue(&mut self, release_ids: &[u64], source: Source) -> Result<u64, CommunityError> {
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
    pub fn next_due(&self, now: i64) -> Result<Option<u64>, CommunityError> {
        let fail = |error| failure(&self.path, error);
        let never = self
            .connection
            .query_row(NEVER_FETCHED, [now], |row| row.get(0))
            .optional()
            .map_err(fail)?;
        if never.is_some() {
            return Ok(never);
        }
        self.connection
            .query_row(REFRESH, [refresh_cutoff(now), now], |row| row.get(0))
            .optional()
            .map_err(fail)
    }

    /// Records the values fetched for a release at time `now`, replacing older ones.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn record(
        &mut self,
        release_id: u64,
        values: Values,
        now: i64,
    ) -> Result<(), CommunityError> {
        self.replace_result(
            release_id,
            "INSERT OR REPLACE INTO result
             (release_id, fetched_at, gone, rating_average, rating_count, have, want)
             VALUES (?1, ?2, 0, ?3, ?4, ?5, ?6)",
            params![
                release_id,
                now,
                values.rating_average,
                values.rating_count,
                values.have,
                values.want
            ],
        )
    }

    /// Records that the API answered 404 for a release at time `now`. Its old values,
    /// if any, are dropped.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn record_gone(&mut self, release_id: u64, now: i64) -> Result<(), CommunityError> {
        self.replace_result(
            release_id,
            "INSERT OR REPLACE INTO result (release_id, fetched_at, gone) VALUES (?1, ?2, 1)",
            params![release_id, now],
        )
    }

    /// Counts a release problem for a release and sets the last error to `problem`, naming the
    /// release, in one transaction. The release waits to be tried again: a minute after the
    /// first, doubling for each one after it, to a day. At the `MAX_ATTEMPTS`th it is marked
    /// failed instead, keeping the values of an earlier fetch, and comes due at the
    /// refresh. `record` and `record_gone` clear the count.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn record_failure(
        &mut self,
        release_id: u64,
        problem: &str,
        now: i64,
    ) -> Result<Failure, CommunityError> {
        let fail = |error| failure(&self.path, error);
        let transaction = self.connection.transaction().map_err(fail)?;
        let attempts: u64 = transaction
            .query_row(
                "SELECT attempts FROM retry WHERE release_id = ?1",
                [release_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(fail)?
            .unwrap_or(0)
            + 1;
        let outcome = if attempts >= MAX_ATTEMPTS {
            mark_failed(&transaction, release_id, problem, now).map_err(fail)?;
            Failure::Failed
        } else {
            let wait = retry_wait(attempts);
            transaction
                .execute(
                    "INSERT OR REPLACE INTO retry (release_id, attempts, retry_at, last_error)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        release_id,
                        attempts,
                        now + wait.as_secs().cast_signed(),
                        problem
                    ],
                )
                .map_err(fail)?;
            Failure::Retry(attempts)
        };
        set_last_error(&transaction, &release_error(release_id, problem), now).map_err(fail)?;
        transaction.commit().map_err(fail)?;
        Ok(outcome)
    }

    /// Replaces a release's `result` row and clears its retry row, together.
    fn replace_result(
        &mut self,
        release_id: u64,
        sql: &str,
        values: impl rusqlite::Params,
    ) -> Result<(), CommunityError> {
        let fail = |error| failure(&self.path, error);
        let transaction = self.connection.transaction().map_err(fail)?;
        transaction.execute(sql, values).map_err(fail)?;
        transaction
            .execute("DELETE FROM retry WHERE release_id = ?1", [release_id])
            .map_err(fail)?;
        mark_done(&transaction, release_id).map_err(fail)?;
        transaction.commit().map_err(fail)
    }

    /// Records an error for `sync status`, replacing the previous one.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn record_error(&mut self, message: &str, now: i64) -> Result<(), CommunityError> {
        let fail = |error| failure(&self.path, error);
        let transaction = self.connection.transaction().map_err(fail)?;
        set_last_error(&transaction, message, now).map_err(fail)?;
        transaction.commit().map_err(fail)
    }

    /// Sets or clears the pause flag, which every running sync reads before each request.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn set_paused(&self, paused: bool) -> Result<(), CommunityError> {
        self.connection
            .execute(SET_STATE, params![PAUSED, paused])
            .map(drop)
            .map_err(|error| failure(&self.path, error))
    }

    /// The pause flag.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn paused(&self) -> Result<bool, CommunityError> {
        Ok(self.state(PAUSED)?.unwrap_or(false))
    }

    fn failed_releases(&self) -> Result<Vec<(u64, String)>, CommunityError> {
        let fail = |error| failure(&self.path, error);
        let mut statement = self
            .connection
            .prepare(
                "SELECT release_id, error FROM result WHERE failed = 1
                 ORDER BY fetched_at DESC, release_id LIMIT ?1",
            )
            .map_err(fail)?;
        statement
            .query_map([FAILED_SHOWN], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(fail)?
            .collect::<rusqlite::Result<_>>()
            .map_err(fail)
    }

    fn state<T: rusqlite::types::FromSql>(&self, key: &str) -> Result<Option<T>, CommunityError> {
        self.connection
            .query_row("SELECT value FROM state WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()
            .map_err(|error| failure(&self.path, error))
    }

    /// The counts and state for `sync status` at time `now`.
    ///
    /// # Errors
    ///
    /// When a read fails.
    pub fn progress(&self, now: i64) -> Result<Progress, CommunityError> {
        let fail = |error| failure(&self.path, error);
        let count = |sql: &str| -> Result<u64, CommunityError> {
            self.connection
                .query_row(sql, [], |row| row.get(0))
                .map_err(fail)
        };
        let last_error = match (self.state(LAST_ERROR)?, self.state(LAST_ERROR_AT)?) {
            (Some(message), Some(at)) => Some((at, message)),
            _ => None,
        };
        Ok(Progress {
            queued: count("SELECT count(*) FROM queue WHERE done = 0")?,
            with_values: count("SELECT count(*) FROM result WHERE rating_average IS NOT NULL")?,
            failed: count("SELECT count(*) FROM result WHERE failed = 1")?,
            gone: count("SELECT count(*) FROM result WHERE gone = 1")?,
            due: self
                .connection
                .query_row(
                    "SELECT count(*) FROM result WHERE gone = 0 AND fetched_at <= ?1",
                    [refresh_cutoff(now)],
                    |row| row.get(0),
                )
                .map_err(fail)?,
            paused: self.paused()?,
            last_error,
            failures: self.failed_releases()?,
        })
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

/// Opens the file and sets the connection up.
fn connect(path: &Path) -> rusqlite::Result<Connection> {
    let connection = Connection::open(path)?;
    connection.busy_timeout(BUSY_TIMEOUT)?;
    // WAL lets `sync status` read while a sync writes.
    connection.pragma_update(None, "journal_mode", "wal")?;
    Ok(connection)
}

/// Whether the file has tables or indexes and they aren't `SCHEMA`'s. An empty file is
/// new, not another shape. Reads only.
fn has_other_shape(connection: &Connection) -> rusqlite::Result<bool> {
    let found = objects(connection)?;
    Ok(!found.is_empty() && found != objects(&expected_connection()?)?)
}

/// An in-memory database with `SCHEMA`, to read the objects the file should have from.
fn expected_connection() -> rusqlite::Result<Connection> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(SCHEMA)?;
    Ok(connection)
}

/// The tables and indexes in the database as (type, name, sql), with the sql's whitespace
/// normalized, sorted. SQLite's own objects (`sqlite_*`) are left out.
fn objects(connection: &Connection) -> rusqlite::Result<Vec<(String, String, String)>> {
    let mut statement = connection.prepare(
        "SELECT type, name, sql FROM sqlite_master WHERE name NOT LIKE 'sqlite\\_%' ESCAPE '\\'
         ORDER BY type, name",
    )?;
    statement
        .query_map([], |row| {
            let sql: String = row.get(2)?;
            Ok((
                row.get(0)?,
                row.get(1)?,
                sql.split_whitespace().collect::<Vec<_>>().join(" "),
            ))
        })?
        .collect()
}

/// The last error for a problem with a release: the text `sync status` shows and the log
/// repeats.
#[must_use]
pub fn release_error(release_id: u64, problem: &str) -> String {
    format!("release {release_id}: {problem}")
}

/// Replaces the last error and its time.
fn set_last_error(connection: &Connection, message: &str, now: i64) -> rusqlite::Result<()> {
    connection.execute(SET_STATE, params![LAST_ERROR, message])?;
    connection.execute(SET_STATE, params![LAST_ERROR_AT, now])?;
    Ok(())
}

/// Sets a release's `result` row to failed, keeping any values, clears its retry row and
/// takes it out of the never-fetched queue.
fn mark_failed(
    connection: &Connection,
    release_id: u64,
    problem: &str,
    now: i64,
) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO result (release_id, fetched_at, gone, failed, error)
         VALUES (?1, ?2, 0, 1, ?3)
         ON CONFLICT (release_id) DO UPDATE SET
             fetched_at = excluded.fetched_at, failed = 1, error = excluded.error",
        params![release_id, now, problem],
    )?;
    connection.execute("DELETE FROM retry WHERE release_id = ?1", [release_id])?;
    mark_done(connection, release_id)
}

const SET_STATE: &str = "INSERT OR REPLACE INTO state (key, value) VALUES (?1, ?2)";

fn failure(path: &Path, error: rusqlite::Error) -> CommunityError {
    CommunityError {
        path: path.to_path_buf(),
        error: Box::new(error),
    }
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

    use super::{Community, NEVER_FETCHED, Values};

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
        };
        db.record(7, values, 100).unwrap();
        for at in 200..210 {
            db.record_failure(7, "Discogs answered 503", at).unwrap();
        }
        let (failed, error, have, at): (i64, String, i64, i64) = db
            .connection
            .query_row(
                "SELECT failed, error, have, fetched_at FROM result WHERE release_id = 7",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            (failed, error.as_str(), have, at),
            (1, "Discogs answered 503", 1200, 209)
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
    fn records_the_values_under_their_release_id() {
        let dir = TempDir::new("record");
        let mut db = Community::open(&dir.0).unwrap();
        db.queue(&[7], super::Source::Library).unwrap();
        let values = Values {
            rating_average: 4.25,
            rating_count: 31,
            have: 1200,
            want: 640,
        };
        db.record(7, values, 100).unwrap();
        let stored = db
            .connection
            .query_row(
                "SELECT rating_average, rating_count, have, want, fetched_at FROM result
                 WHERE release_id = 7",
                [],
                |row| {
                    Ok((
                        Values {
                            rating_average: row.get(0)?,
                            rating_count: row.get(1)?,
                            have: row.get(2)?,
                            want: row.get(3)?,
                        },
                        row.get::<_, i64>(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(stored, (values, 100));
    }
}
