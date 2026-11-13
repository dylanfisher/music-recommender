//! The owner's feedback on recommendations, "not interested" and "already have" for a release
//! or master, kept in a database of its own (ADR 0024). Only setting a verdict creates the
//! file: listing and undoing read an absent one as no feedback. The lists leave out every
//! release and master with a mark, read as each page is (ADR 0028).

use std::collections::HashSet;
use std::fmt;
use std::path::Path;

use catalog::community::Reading;
use catalog::error::DatabaseError;
use catalog::feedback::{Feedback, Page};
// The store's types, for the API, which reaches the store through here.
pub use catalog::feedback::{Kind, Mark, Verdict};
use config::Config;

/// Why the feedback couldn't be read or written.
#[derive(Debug)]
pub enum FeedbackError {
    /// The feedback database failed.
    Database(DatabaseError),
    /// The file's tables are from another build, so it wasn't read.
    OtherShape,
}

impl fmt::Display for FeedbackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database(error) => error.fmt(f),
            Self::OtherShape => write!(
                f,
                "feedback database: its tables are from another version; the file will be \
                 started again when a verdict is next set"
            ),
        }
    }
}

impl std::error::Error for FeedbackError {}

impl From<DatabaseError> for FeedbackError {
    fn from(error: DatabaseError) -> Self {
        Self::Database(error)
    }
}

/// Sets the verdict on a release or master at `now`, creating the database if there is none.
///
/// # Errors
///
/// When the database fails.
pub fn set(
    config: &Config,
    kind: Kind,
    id: i64,
    verdict: Verdict,
    now: i64,
) -> Result<Mark, FeedbackError> {
    Ok(Feedback::open(&config.data_dir)?.set(kind, id, verdict, now)?)
}

/// Removes the verdict on a release or master, and says whether there was one. With no
/// database there is none, and no file is created.
///
/// # Errors
///
/// When the database fails, or its file is of another shape.
pub fn undo(config: &Config, kind: Kind, id: i64) -> Result<bool, FeedbackError> {
    match Feedback::open_read_only(&config.data_dir)? {
        Reading::Missing => Ok(false),
        Reading::OtherShape => Err(FeedbackError::OtherShape),
        // The read-only connection can't delete, so the file is opened again to write.
        Reading::Open(_) => Ok(Feedback::open(&config.data_dir)?.undo(kind, id)?),
    }
}

/// The marks with a seq above `after`, a page at a time. With no database the page is
/// empty, and no file is created.
///
/// # Errors
///
/// When the database fails, or its file is of another shape.
pub fn list(config: &Config, after: i64) -> Result<Page, FeedbackError> {
    match Feedback::open_read_only(&config.data_dir)? {
        Reading::Missing => Ok(Page {
            marks: Vec::new(),
            more: false,
        }),
        Reading::OtherShape => Err(FeedbackError::OtherShape),
        Reading::Open(db) => Ok(db.list(after)?),
    }
}

/// Every release and master with a mark, whatever its verdict, for the lists to leave out.
/// With no database there are none, and no file is created.
///
/// # Errors
///
/// When the database fails, or its file is of another shape.
pub(crate) fn marked(data_dir: &Path) -> Result<HashSet<(Kind, u64)>, FeedbackError> {
    let db = match Feedback::open_read_only(data_dir)? {
        Reading::Missing => return Ok(HashSet::new()),
        Reading::OtherShape => return Err(FeedbackError::OtherShape),
        Reading::Open(db) => db,
    };
    let mut marked = HashSet::new();
    let mut after = 0;
    loop {
        let page = db.list(after)?;
        // A mark's id is from 1, as the routes and the store take it.
        marked.extend(
            page.marks
                .iter()
                .map(|mark| (mark.kind, mark.id.cast_unsigned())),
        );
        match page.marks.last() {
            Some(last) if page.more => after = last.seq,
            _ => return Ok(marked),
        }
    }
}
