//! The community values the sync stored, read for the player's library columns
//! (`POST /v1/community`).

use std::fmt;

use catalog::community::{Community, Reading};
pub use catalog::community::{Stored, Values};
use catalog::error::DatabaseError;
use config::Config;

use crate::status::OTHER_SHAPE;

/// Why the stored values couldn't be read.
#[derive(Debug)]
pub enum ReadError {
    /// The community database failed.
    Database(DatabaseError),
    /// The file's tables are from another build, so it wasn't read.
    OtherShape,
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database(error) => error.fmt(f),
            Self::OtherShape => write!(f, "community database: {OTHER_SHAPE}"),
        }
    }
}

impl std::error::Error for ReadError {}

/// What the community database holds for each of `release_ids`, in their order. It only
/// reads: with no database yet, every id is `Stored::NotQueued`, and no file is created.
///
/// # Errors
///
/// When the database fails, or its file is of another shape.
pub fn stored(config: &Config, release_ids: &[u64]) -> Result<Vec<Stored>, ReadError> {
    match Community::open_read_only(&config.data_dir).map_err(ReadError::Database)? {
        Reading::Missing => Ok(vec![Stored::NotQueued; release_ids.len()]),
        Reading::OtherShape => Err(ReadError::OtherShape),
        Reading::Open(db) => db.stored(release_ids).map_err(ReadError::Database),
    }
}
