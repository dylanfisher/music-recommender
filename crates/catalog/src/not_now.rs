//! Rediscover's not-now marks: each album, by its id, left out until a time, in
//! `rediscover.sqlite` in the data directory (ADR 0034). A file of its own, so neither the
//! catalog's rebuild, a reset of the community database nor the feedback's file touches it,
//! and a change to its shape resets only the marks. Every time is passed in, in seconds since
//! the Unix epoch.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, TransactionBehavior, params};

use crate::error::DatabaseError;
use crate::sqlite::{self, Reading};

/// The database's file name inside the data directory.
const FILE_NAME: &str = "rediscover.sqlite";

/// The database's name in its errors.
const NAME: &str = "rediscover";

/// The lock file `open` holds while it checks, deletes and creates the database file.
const OPEN_LOCK_FILE: &str = "rediscover.open.lock";

/// One row per album with a mark.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS not_now (
    id TEXT PRIMARY KEY,
    until INTEGER NOT NULL
);
";

/// An open not-now database.
pub struct NotNow {
    path: PathBuf,
    connection: Connection,
}

impl NotNow {
    /// Opens the not-now database in `data_dir`, creating the file and its table if they
    /// don't exist. A file of another shape is deleted and started again, and a line says so
    /// on stderr.
    ///
    /// # Errors
    ///
    /// When `data_dir` isn't a directory, or the file can't be opened or set up.
    pub fn open(data_dir: &Path) -> Result<Self, DatabaseError> {
        let path = data_dir.join(FILE_NAME);
        let opened = sqlite::open(data_dir, &SPEC, sqlite::BUSY_TIMEOUT)
            .map_err(|error| DatabaseError::new(NAME, &path, error.boxed()))?;
        if opened.reset {
            eprintln!(
                "rediscover database {}: its tables are from another version, so the file was \
                 deleted and started again with no not-now marks",
                path.display()
            );
        }
        Ok(Self {
            path,
            connection: opened.connection,
        })
    }

    /// Opens the not-now database in `data_dir` to read, and only read: it creates no file,
    /// deletes none and doesn't reset one of another shape (see `Reading`). The writing
    /// methods fail on this connection.
    ///
    /// # Errors
    ///
    /// When `data_dir` isn't a directory, the open lock can't be taken in time, or the file
    /// can't be opened.
    pub fn open_read_only(data_dir: &Path) -> Result<Reading<Self>, DatabaseError> {
        let path = data_dir.join(FILE_NAME);
        let reading = sqlite::open_read_only(data_dir, &SPEC, sqlite::BUSY_TIMEOUT)
            .map_err(|error| DatabaseError::new(NAME, &path, error.boxed()))?;
        Ok(reading.map(|connection| Self { path, connection }))
    }

    /// Leaves album `id` out until `until`, replacing a mark it had. The marks that ended by
    /// `now` are removed with it.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn set(&mut self, id: &str, until: i64, now: i64) -> Result<(), DatabaseError> {
        let fail = |error| failure(&self.path, error);
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(fail)?;
        transaction
            .execute("DELETE FROM not_now WHERE until <= ?1", params![now])
            .map_err(fail)?;
        transaction
            .execute(
                "INSERT OR REPLACE INTO not_now (id, until) VALUES (?1, ?2)",
                params![id, until],
            )
            .map_err(fail)?;
        transaction.commit().map_err(fail)
    }

    /// Removes album `id`'s mark. Returns whether there was one, ended or not.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn undo(&mut self, id: &str) -> Result<bool, DatabaseError> {
        self.connection
            .execute("DELETE FROM not_now WHERE id = ?1", params![id])
            .map(|removed| removed > 0)
            .map_err(|error| failure(&self.path, error))
    }

    /// The ids of the albums whose mark hasn't ended at `now`, in order.
    ///
    /// # Errors
    ///
    /// When the read fails.
    pub fn marked(&self, now: i64) -> Result<Vec<String>, DatabaseError> {
        let fail = |error| failure(&self.path, error);
        self.connection
            .prepare_cached("SELECT id FROM not_now WHERE until > ?1 ORDER BY id")
            .map_err(fail)?
            .query_map(params![now], |row| row.get(0))
            .map_err(fail)?
            .collect::<rusqlite::Result<_>>()
            .map_err(fail)
    }
}

/// The objects `SCHEMA` creates, read the first time they are needed.
static EXPECTED: sqlite::Expected = sqlite::Expected::new();

/// Where the not-now database is and what it holds.
static SPEC: sqlite::Spec<'static> = sqlite::Spec {
    file_name: FILE_NAME,
    lock_file: OPEN_LOCK_FILE,
    schema: SCHEMA,
    later: "",
    expected: &EXPECTED,
};

fn failure(path: &Path, error: rusqlite::Error) -> DatabaseError {
    DatabaseError::new(NAME, path, error)
}
