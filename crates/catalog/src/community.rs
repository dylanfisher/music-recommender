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

/// A release is recorded as failed after this many bad replies since its last success...
const MAX_FAILURES: u64 = 5;

/// ...when at least this long has passed since the first of them, so a short outage of
/// Discogs (a few minutes of backoff) doesn't fail a release for the whole refresh.
const FAILURE_SPAN: Duration = Duration::from_hours(1);

/// A streak whose first bad reply is older than this starts over: replies that far apart
/// aren't one outage.
const STREAK_EXPIRES_AFTER: Duration = Duration::from_hours(24);

/// `queue` holds every release id ever queued, in order; `result` one row per release
/// fetched, replaced on each fetch. A release is never fetched while it has no `result`
/// row: `queue.done` is 1 exactly when it has one, set in the same transaction as the
/// row, so the never-fetched query reads only the releases left. A failed row keeps the
/// values of the last fetch, if any, and adds the last error. `failures` counts the
/// transient bad replies since the release's last success, and when the first came.
/// `state` has no column type, so integers stay integers.
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
CREATE TABLE IF NOT EXISTS failures (
    release_id INTEGER PRIMARY KEY REFERENCES queue (release_id),
    count INTEGER NOT NULL,
    first_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS state (
    key TEXT PRIMARY KEY,
    value NOT NULL
);
";

const NEVER_FETCHED: &str = "FROM queue q WHERE q.done = 0";

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

/// What a bad reply did to a release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The release will be tried again. Holds its bad replies since its last success.
    Retry(u64),
    /// The release is recorded as failed and leaves the queue until the refresh.
    Failed,
}

/// Counts and state for `sync status`.
#[derive(Debug, PartialEq, Eq)]
pub struct Progress {
    /// Queued and never fetched.
    pub queued: u64,
    /// With values from a fetch, including failed releases that had some.
    pub fetched: u64,
    /// Given up on: a permanent reply, or `MAX_FAILURES` transient ones over
    /// `FAILURE_SPAN`. Values from an earlier fetch are kept. Fetched again on refresh.
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
    /// A file whose tables have other columns than `SCHEMA` (an older build wrote it) is
    /// discarded: every table is dropped and recreated empty, the queue too, since the
    /// profile can be loaded again, and a line saying so goes to stderr.
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
        let mut connection = connect(&path).map_err(fail)?;
        // Pre-release only (AGENTS.md): delete this check with the pre-release line there.
        if has_other_shape(&connection).map_err(fail)? {
            drop(connection);
            for suffix in ["", "-wal", "-shm"] {
                let mut file = path.clone().into_os_string();
                file.push(suffix);
                match std::fs::remove_file(&file) {
                    Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                        return Err(CommunityError {
                            path: path.clone(),
                            error: Box::new(error),
                        });
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

    /// Counts a transient bad reply (a 5xx, a body that can't be read) for a release. Once
    /// it has `MAX_FAILURES` since its last success and the first was at least
    /// `FAILURE_SPAN` ago, the release is recorded as failed, as by `record_failed`. A
    /// streak whose first reply is more than `STREAK_EXPIRES_AFTER` old starts over with
    /// this one. A network error or a 429 isn't the release's fault: it isn't counted and
    /// doesn't clear the count. `record` and `record_gone` clear it.
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
                "INSERT INTO failures (release_id, count, first_at) VALUES (?1, 1, ?2)
                 ON CONFLICT (release_id) DO UPDATE SET
                     count = CASE WHEN ?2 - first_at > ?3 THEN 1 ELSE count + 1 END,
                     first_at = CASE WHEN ?2 - first_at > ?3 THEN ?2 ELSE first_at END",
                params![
                    release_id,
                    now,
                    STREAK_EXPIRES_AFTER.as_secs().cast_signed()
                ],
            )
            .map_err(fail)?;
        let (count, first_at): (u64, i64) = transaction
            .query_row(
                "SELECT count, first_at FROM failures WHERE release_id = ?1",
                [release_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(fail)?;
        let span = FAILURE_SPAN.as_secs().cast_signed();
        let outcome = if count >= MAX_FAILURES && now - first_at >= span {
            mark_failed(&transaction, release_id, message, now).map_err(fail)?;
            Failure::Failed
        } else {
            Failure::Retry(count)
        };
        transaction.commit().map_err(fail)?;
        Ok(outcome)
    }

    /// Records a release as failed at time `now` with `message`, at once: for a permanent
    /// reply (a redirect, a 4xx other than 401, 403, 404 and 429), which asking again
    /// won't change. Values from an earlier fetch are kept; the release comes due again at
    /// the refresh.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn record_failed(
        &mut self,
        release_id: u64,
        message: &str,
        now: i64,
    ) -> Result<(), CommunityError> {
        let fail = |error| failure(&self.path, error);
        let transaction = self.connection.transaction().map_err(fail)?;
        mark_failed(&transaction, release_id, message, now).map_err(fail)?;
        transaction.commit().map_err(fail)
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
            queued: count(&format!("SELECT count(*) {NEVER_FETCHED}"))?,
            fetched: count("SELECT count(*) FROM result WHERE rating_average IS NOT NULL")?,
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

/// Whether a table in the file has other columns than the same table in `SCHEMA`. Tables
/// the file lacks don't count: `SCHEMA` creates them. Reads only.
fn has_other_shape(connection: &Connection) -> rusqlite::Result<bool> {
    let expected = shape(&expected_connection()?)?;
    Ok(shape(connection)?.iter().any(|(table, columns)| {
        expected
            .iter()
            .any(|(name, wanted)| name == table && wanted != columns)
    }))
}

/// Per table: its name and its columns' name, type, not-null flag and primary-key position.
type Shape = Vec<(String, Vec<(String, String, i64, i64)>)>;

/// An in-memory database with `SCHEMA`, to read the shape the tables should have from.
fn expected_connection() -> rusqlite::Result<Connection> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(SCHEMA)?;
    Ok(connection)
}

/// The shape of every table in the database, in name order.
fn shape(connection: &Connection) -> rusqlite::Result<Shape> {
    let mut tables = connection.prepare(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
         ORDER BY name",
    )?;
    let names: Vec<String> = tables
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    names
        .into_iter()
        .map(|name| {
            let mut columns = connection.prepare(&format!("PRAGMA table_info({name})"))?;
            let columns = columns
                .query_map([], |row| {
                    Ok((row.get(1)?, row.get(2)?, row.get(3)?, row.get(5)?))
                })?
                .collect::<rusqlite::Result<_>>()?;
            Ok((name, columns))
        })
        .collect()
}

/// Sets a release's `result` row to failed, keeping any values, clears its failure count
/// and takes it out of the never-fetched queue.
fn mark_failed(
    connection: &Connection,
    release_id: u64,
    message: &str,
    now: i64,
) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO result (release_id, fetched_at, gone, failed, error)
         VALUES (?1, ?2, 0, 1, ?3)
         ON CONFLICT (release_id) DO UPDATE SET
             fetched_at = excluded.fetched_at, failed = 1, error = excluded.error",
        params![release_id, now, message],
    )?;
    connection.execute("DELETE FROM failures WHERE release_id = ?1", [release_id])?;
    mark_done(connection, release_id)
}

const SET_STATE: &str = "INSERT OR REPLACE INTO state (key, value) VALUES (?1, ?2)";

fn failure(path: &Path, error: rusqlite::Error) -> CommunityError {
    CommunityError {
        path: path.to_path_buf(),
        error: Box::new(error),
    }
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
        for at in [200, 201, 202, 203, 200 + 3600] {
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
            (1, "Discogs answered 503", 1200, 3800)
        );
    }

    #[test]
    fn next_due_reads_the_queue_in_index_order_without_sorting() {
        let dir = TempDir::new("plan");
        let db = Community::open(&dir.0).unwrap();
        let never = plan(
            &db,
            &format!("SELECT q.release_id {NEVER_FETCHED} ORDER BY q.source, q.seq LIMIT 1"),
        );
        assert!(
            never.contains("SCAN q USING INDEX queue_todo")
                || never.contains("SCAN q USING COVERING INDEX queue_todo"),
            "{never}"
        );
        assert!(!never.contains("TEMP B-TREE"), "{never}");
        let refresh = plan(
            &db,
            "SELECT release_id FROM result WHERE gone = 0 AND fetched_at <= 5
             ORDER BY fetched_at, release_id LIMIT 1",
        );
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
