//! The owner's feedback on recommendations: "not interested" and "already have" for a Discogs
//! release or master, in `feedback.sqlite` in the data directory (ADR 0024). Its own file, so
//! neither the catalog's monthly rebuild nor a reset of the community database touches it.
//! Every time is passed in, in seconds since the Unix epoch.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::download::{named, named_enum};
use crate::error::DatabaseError;
use crate::sqlite::{self, Reading};

/// The database's file name inside the data directory.
const FILE_NAME: &str = "feedback.sqlite";

/// The database's name in its errors.
const NAME: &str = "feedback";

/// The lock file `open` holds while it checks, deletes and creates the database file.
const OPEN_LOCK_FILE: &str = "feedback.open.lock";

/// The most marks `list` returns at once. A mark is under 100 bytes of JSON, so a page stays
/// under 50 KB.
pub(crate) const PAGE: usize = 500;

/// One row per release or master with a verdict. `seq` is `AUTOINCREMENT`, so a seq is never
/// given twice, even after the row holding the highest is deleted: a client paging past a seq
/// still sees every mark set after it.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS mark (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    kind TEXT NOT NULL,
    id INTEGER NOT NULL,
    verdict TEXT NOT NULL,
    at INTEGER NOT NULL,
    UNIQUE (kind, id)
);
";

named_enum! {
    /// What a mark is on: Discogs keeps releases and masters apart, each with its own ids.
    Kind {
        /// A release.
        Release = "release",
        /// A master, standing for every release of it.
        Master = "master",
    }
}

named_enum! {
    /// What the owner said of it.
    Verdict {
        /// Not wanted in any list.
        NotInterested = "not_interested",
        /// Owned outside the library the profile describes.
        AlreadyHave = "already_have",
    }
}

/// A verdict on a release or master.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mark {
    /// Its place in `list`: every mark set takes the next one.
    pub seq: i64,
    /// A release or a master.
    pub kind: Kind,
    /// The Discogs release or master id.
    pub id: i64,
    /// What the owner said.
    pub verdict: Verdict,
    /// When it was set.
    pub at: i64,
}

/// A page of `list`.
#[derive(Debug, PartialEq, Eq)]
pub struct Page {
    /// The marks, ascending by `seq`.
    pub marks: Vec<Mark>,
    /// Whether marks remain past the last one.
    pub more: bool,
}

/// An open feedback database.
pub struct Feedback {
    path: PathBuf,
    connection: Connection,
}

impl Feedback {
    /// Opens the feedback database in `data_dir`, creating the file and its table if they
    /// don't exist. A file of another shape is deleted and started again, and a line says so
    /// on stderr.
    ///
    /// # Errors
    ///
    /// When `data_dir` isn't a directory, or the file can't be opened or set up.
    pub fn open(data_dir: &Path) -> Result<Self, DatabaseError> {
        let path = data_dir.join(FILE_NAME);
        let opened = sqlite::open(data_dir, &spec(), sqlite::BUSY_TIMEOUT)
            .map_err(|error| DatabaseError::new(NAME, &path, error.boxed()))?;
        if opened.reset {
            eprintln!(
                "feedback database {}: its tables are from another version, so the file was \
                 deleted and started again with no feedback",
                path.display()
            );
        }
        Ok(Self {
            path,
            connection: opened.connection,
        })
    }

    /// Opens the feedback database in `data_dir` to read, and only read: it creates no file,
    /// deletes none and doesn't reset one of another shape (see `Reading`). The writing
    /// methods fail on this connection.
    ///
    /// # Errors
    ///
    /// When `data_dir` isn't a directory, the open lock can't be taken in time, or the file
    /// can't be opened.
    pub fn open_read_only(data_dir: &Path) -> Result<Reading<Self>, DatabaseError> {
        let path = data_dir.join(FILE_NAME);
        let reading = sqlite::open_read_only(data_dir, &spec(), sqlite::BUSY_TIMEOUT)
            .map_err(|error| DatabaseError::new(NAME, &path, error.boxed()))?;
        Ok(reading.map(|connection| Self { path, connection }))
    }

    /// Sets the verdict on a release or master at `now`, and returns its mark. The same
    /// verdict again leaves the mark as it was; another replaces it with the next seq.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn set(
        &mut self,
        kind: Kind,
        id: i64,
        verdict: Verdict,
        now: i64,
    ) -> Result<Mark, DatabaseError> {
        let fail = |error| failure(&self.path, error);
        // Immediate, so the write lock is taken before the read: a deferred transaction that
        // read and then found another connection had written would fail at its write
        // instead of waiting for the busy timeout.
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(fail)?;
        let existing = transaction
            .query_row(
                "SELECT seq, kind, id, verdict, at FROM mark WHERE kind = ?1 AND id = ?2",
                params![kind.name(), id],
                mark,
            )
            .optional()
            .map_err(fail)?;
        if let Some(existing) = existing.filter(|existing| existing.verdict == verdict) {
            return Ok(existing);
        }
        transaction
            .execute(
                "DELETE FROM mark WHERE kind = ?1 AND id = ?2",
                params![kind.name(), id],
            )
            .map_err(fail)?;
        transaction
            .execute(
                "INSERT INTO mark (kind, id, verdict, at) VALUES (?1, ?2, ?3, ?4)",
                params![kind.name(), id, verdict.name(), now],
            )
            .map_err(fail)?;
        let seq = transaction.last_insert_rowid();
        transaction.commit().map_err(fail)?;
        Ok(Mark {
            seq,
            kind,
            id,
            verdict,
            at: now,
        })
    }

    /// Removes the verdict on a release or master. Returns whether there was one.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub fn undo(&mut self, kind: Kind, id: i64) -> Result<bool, DatabaseError> {
        self.connection
            .execute(
                "DELETE FROM mark WHERE kind = ?1 AND id = ?2",
                params![kind.name(), id],
            )
            .map(|removed| removed > 0)
            .map_err(|error| failure(&self.path, error))
    }

    /// The marks with a seq above `after`, ascending, at most [`PAGE`].
    ///
    /// # Errors
    ///
    /// When the read fails, or a row holds a kind or verdict this build doesn't know.
    pub fn list(&self, after: i64) -> Result<Page, DatabaseError> {
        let fail = |error| failure(&self.path, error);
        let mut marks = self
            .connection
            .prepare_cached(
                "SELECT seq, kind, id, verdict, at FROM mark WHERE seq > ?1 ORDER BY seq LIMIT ?2",
            )
            .map_err(fail)?
            .query_map(params![after, PAGE + 1], mark)
            .map_err(fail)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(fail)?;
        let more = marks.len() > PAGE;
        marks.truncate(PAGE);
        Ok(Page { marks, more })
    }
}

/// Where the feedback database is and what it holds.
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

/// A `mark` row as (seq, kind, id, verdict, at).
fn mark(row: &rusqlite::Row<'_>) -> rusqlite::Result<Mark> {
    Ok(Mark {
        seq: row.get(0)?,
        kind: named(&row.get::<_, String>(1)?, Kind::parse)?,
        id: row.get(2)?,
        verdict: named(&row.get::<_, String>(3)?, Verdict::parse)?,
        at: row.get(4)?,
    })
}

fn failure(path: &Path, error: rusqlite::Error) -> DatabaseError {
    DatabaseError::new(NAME, path, error)
}
