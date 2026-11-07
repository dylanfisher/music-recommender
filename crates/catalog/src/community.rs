//! The community database: the sync's queue of Discogs release ids, the ratings and
//! have/want counts fetched for them, and the sync's state (the pause flag, the last
//! error). Its own file, which the catalog's monthly rebuild never replaces (ADR 0004).
//!
//! Every time is passed in, in seconds since the Unix epoch, so callers and tests choose
//! the clock.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};

/// The database's file name inside the data directory.
const FILE_NAME: &str = "community.sqlite";

/// A value this old is fetched again, behind every release never fetched.
const REFRESH_AFTER: Duration = Duration::from_hours(90 * 24);

/// How long a write waits for another process (a `sync pause` during a sync) to finish.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// A release with this many bad replies in a row is recorded as failed.
const MAX_FAILURES: u64 = 5;

/// `queue` holds every release id ever queued, in order; `result` one row per release
/// fetched, replaced on each fetch. A release is never fetched while it has no `result`
/// row. A failed row has no values, only the last error; `failures` counts the bad
/// replies in a row for a release that has no failed row yet. `state` has no column
/// type, so integers stay integers.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS queue (
    seq INTEGER PRIMARY KEY,
    release_id INTEGER NOT NULL UNIQUE,
    source INTEGER NOT NULL
);
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
CREATE TABLE IF NOT EXISTS failures (
    release_id INTEGER PRIMARY KEY REFERENCES queue (release_id),
    count INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS state (
    key TEXT PRIMARY KEY,
    value NOT NULL
);
";

const NEVER_FETCHED: &str =
    "FROM queue q WHERE NOT EXISTS (SELECT 1 FROM result r WHERE r.release_id = q.release_id)";

const PAUSED: &str = "paused";
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

/// What a bad reply did to a release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The release will be tried again. Holds its bad replies in a row so far.
    Retry(u64),
    /// That was the last one allowed: the release is recorded as failed and leaves the
    /// queue.
    Failed,
}

/// Counts and state for `sync status`.
#[derive(Debug, PartialEq, Eq)]
pub struct Progress {
    /// Queued and never fetched.
    pub queued: u64,
    /// Fetched, with values.
    pub fetched: u64,
    /// Given up on after `MAX_FAILURES` bad replies in a row. Fetched again on refresh.
    pub failed: u64,
    /// Fetched and answered 404: no longer on Discogs. These aren't fetched again.
    pub gone: u64,
    /// Fetched or failed, at a time older than `REFRESH_AFTER`.
    pub due: u64,
    /// The pause flag.
    pub paused: bool,
    /// The last error recorded and its time, if any.
    pub last_error: Option<(i64, String)>,
}

/// A failed read or write, naming the database file.
#[derive(Debug)]
pub struct CommunityError {
    path: PathBuf,
    error: rusqlite::Error,
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
    /// # Errors
    ///
    /// When `data_dir` isn't a directory, or the file can't be opened or set up.
    pub fn open(data_dir: &Path) -> Result<Self, CommunityError> {
        let path = data_dir.join(FILE_NAME);
        let fail = |error| failure(&path, error);
        if !data_dir.is_dir() {
            return Err(fail(rusqlite::Error::InvalidPath(data_dir.to_path_buf())));
        }
        let connection = Connection::open(&path).map_err(fail)?;
        connection.busy_timeout(BUSY_TIMEOUT).map_err(fail)?;
        // WAL lets `sync status` read while a sync writes.
        connection
            .pragma_update(None, "journal_mode", "wal")
            .map_err(fail)?;
        connection.execute_batch(SCHEMA).map_err(fail)?;
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
    /// candidates and each in the order queued; then the one whose values are oldest, if
    /// they're `REFRESH_AFTER` old. `None` when nothing is due.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn next_due(&self, now: i64) -> Result<Option<u64>, CommunityError> {
        let fail = |error| failure(&self.path, error);
        let never = self
            .connection
            .query_row(
                &format!("SELECT q.release_id {NEVER_FETCHED} ORDER BY q.source, q.seq LIMIT 1"),
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(fail)?;
        if never.is_some() {
            return Ok(never);
        }
        self.connection
            .query_row(
                "SELECT release_id FROM result WHERE gone = 0 AND fetched_at <= ?1
                 ORDER BY fetched_at, release_id LIMIT 1",
                [refresh_cutoff(now)],
                |row| row.get(0),
            )
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

    /// Counts a bad reply (a 5xx, an unexpected status, a body that can't be read) for a
    /// release. At `MAX_FAILURES` in a row the release is recorded as failed at time `now`
    /// with `message`, and any old values are dropped. A network error isn't the
    /// release's fault and isn't counted. `record` and `record_gone` clear the count.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn record_failure(
        &mut self,
        release_id: u64,
        message: &str,
        now: i64,
    ) -> Result<Failure, CommunityError> {
        let fail = |error| failure(&self.path, error);
        let transaction = self.connection.transaction().map_err(fail)?;
        transaction
            .execute(
                "INSERT INTO failures (release_id, count) VALUES (?1, 1)
                 ON CONFLICT (release_id) DO UPDATE SET count = count + 1",
                [release_id],
            )
            .map_err(fail)?;
        let count: u64 = transaction
            .query_row(
                "SELECT count FROM failures WHERE release_id = ?1",
                [release_id],
                |row| row.get(0),
            )
            .map_err(fail)?;
        let outcome = if count >= MAX_FAILURES {
            transaction
                .execute(
                    "INSERT OR REPLACE INTO result (release_id, fetched_at, gone, failed, error)
                     VALUES (?1, ?2, 0, 1, ?3)",
                    params![release_id, now, message],
                )
                .map_err(fail)?;
            transaction
                .execute("DELETE FROM failures WHERE release_id = ?1", [release_id])
                .map_err(fail)?;
            Failure::Failed
        } else {
            Failure::Retry(count)
        };
        transaction.commit().map_err(fail)?;
        Ok(outcome)
    }

    /// Replaces a release's `result` row and clears its failure count, together.
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
            .execute("DELETE FROM failures WHERE release_id = ?1", [release_id])
            .map_err(fail)?;
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
        transaction
            .execute(SET_STATE, params![LAST_ERROR, message])
            .map_err(fail)?;
        transaction
            .execute(SET_STATE, params![LAST_ERROR_AT, now])
            .map_err(fail)?;
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
            queued: count(&format!("SELECT count(*) {NEVER_FETCHED}"))?,
            fetched: count("SELECT count(*) FROM result WHERE gone = 0 AND failed = 0")?,
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
        })
    }
}

const SET_STATE: &str = "INSERT OR REPLACE INTO state (key, value) VALUES (?1, ?2)";

fn failure(path: &Path, error: rusqlite::Error) -> CommunityError {
    CommunityError {
        path: path.to_path_buf(),
        error,
    }
}

/// Values fetched at or before this time are due at `now`.
fn refresh_cutoff(now: i64) -> i64 {
    now - REFRESH_AFTER.as_secs().cast_signed()
}

#[cfg(test)]
mod tests {
    use super::{Community, Values};

    #[test]
    fn records_the_values_under_their_release_id() {
        let dir = std::env::temp_dir().join(format!("catalog-record-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut db = Community::open(&dir).unwrap();
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
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
