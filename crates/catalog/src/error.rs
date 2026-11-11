//! The error every database the service keeps fails with: what failed, naming the database
//! and its file.

use std::fmt;
use std::path::{Path, PathBuf};

/// A failed read or write, naming the database and its file.
#[derive(Debug)]
pub struct DatabaseError {
    /// The database's name for people: `community`, `downloads` or `feedback`.
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
