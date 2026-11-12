//! The error every database the service keeps fails with: what failed, naming the database
//! and its file.

use std::fmt;
use std::path::{Path, PathBuf};

/// A failed read or write, naming the database and its file.
#[derive(Debug)]
pub struct DatabaseError {
    /// The database's name for people: `community`, `downloads`, `feedback` or
    /// `scores`.
    name: &'static str,
    path: PathBuf,
    error: Box<dyn std::error::Error + Send + Sync>,
}

impl DatabaseError {
    /// `error` from the database `name` in the file at `path`.
    pub(crate) fn new(
        name: &'static str,
        path: &Path,
        error: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        Self {
            name,
            path: path.to_path_buf(),
            error: error.into(),
        }
    }

    /// Whether the file is damaged: SQLite says it is no database or is corrupt. Any other
    /// failure (a file that can't be opened now, a busy one, no file handles left) says
    /// nothing about the file's contents.
    #[must_use]
    pub(crate) fn is_damaged(&self) -> bool {
        use rusqlite::ErrorCode::{DatabaseCorrupt, NotADatabase};
        matches!(
            self.error.downcast_ref::<rusqlite::Error>(),
            Some(rusqlite::Error::SqliteFailure(failure, _))
                if matches!(failure.code, NotADatabase | DatabaseCorrupt)
        )
    }
}

impl fmt::Display for DatabaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} database {}: {}",
            self.name,
            self.path.display(),
            self.error
        )
    }
}

impl std::error::Error for DatabaseError {}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::ffi;

    fn sqlite(code: i32) -> DatabaseError {
        let failure = rusqlite::Error::SqliteFailure(ffi::Error::new(code), None);
        DatabaseError::new("scores", Path::new("scores.sqlite"), failure)
    }

    #[test]
    fn only_a_file_that_is_no_database_or_is_corrupt_is_damaged() {
        assert!(sqlite(ffi::SQLITE_NOTADB).is_damaged());
        assert!(sqlite(ffi::SQLITE_CORRUPT).is_damaged());
        assert!(!sqlite(ffi::SQLITE_BUSY).is_damaged());
        assert!(!sqlite(ffi::SQLITE_CANTOPEN).is_damaged());
        let io = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert!(!DatabaseError::new("scores", Path::new("scores.sqlite"), io).is_damaged());
    }
}
