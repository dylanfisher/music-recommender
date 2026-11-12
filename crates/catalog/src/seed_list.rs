//! The seed lists kept on disk (ADR 0028): a seed album's or a seed artist's list, computed
//! when it is first asked for and kept in `seed-lists/` in the data directory, one file a
//! seed, of `scores.sqlite`'s shape with the `computed` row it was scored from, so later
//! pages and a `serve` restarted read it instead of a pass over the catalog.
//!
//! Each list is written as a `.next` file and renamed in whole, as the scores are. One
//! computation runs at a time, under `seed-lists.lock`, and the sweep after it deletes every
//! list whose inputs aren't the current ones and the least recently asked past a cap. When
//! a list was last asked for is its file's modification time, which [`asked`] sets.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::community::{LockError, SyncLock, lock_running_waiting};
use crate::error::DatabaseError;
use crate::score::{Inputs, Scores, Writer, digest};
use crate::sqlite::{self, Reading};

/// The folder of the lists, inside the data directory.
const DIR: &str = "seed-lists";

/// A list's file ends with this.
const EXTENSION: &str = ".sqlite";

/// A list being written ends with this, and is renamed to the list's file when complete.
const NEXT: &str = ".next.sqlite";

/// The lock file a computation holds, so two can't run at once in any process.
const LOCK_FILE: &str = "seed-lists.lock";

/// The lock file a computation also holds, for a status to probe (ADR 0021's pattern).
const RUNNING_FILE: &str = "seed-lists.running";

/// The database's name in its errors.
const NAME: &str = "seed lists";

/// Whose list it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seed<'a> {
    /// The seed album tagged with this release.
    Album(u64),
    /// The seed albums by this artist, by the name folded as the lists fold it.
    Artist(&'a str),
}

impl Seed<'_> {
    /// The list's file name without its extension. An artist's name is kept as its digest,
    /// so any name makes a file name of letters and digits.
    fn stem(&self) -> String {
        match self {
            Self::Album(release_id) => format!("album-{release_id}"),
            Self::Artist(key) => format!("artist-{}", digest(key)),
        }
    }
}

/// Takes the lock a computation of a seed list holds, waiting while another holds it, in
/// this process or another; `waiting` is called once before that wait.
///
/// # Errors
///
/// When a lock file can't be opened or locked.
pub fn lock(data_dir: &Path, waiting: impl FnOnce()) -> Result<SyncLock, LockError> {
    lock_running_waiting(
        data_dir.join(LOCK_FILE),
        data_dir.join(RUNNING_FILE),
        waiting,
    )
}

/// `seed`'s list, opened to read: `Reading::Missing` when none is kept, and
/// `Reading::OtherShape` for a file another version wrote, which the caller reads as none.
///
/// # Errors
///
/// When the file can't be opened.
pub fn open(data_dir: &Path, seed: &Seed<'_>) -> Result<Reading<Scores>, DatabaseError> {
    let dir = data_dir.join(DIR);
    if !dir.is_dir() {
        return Ok(Reading::Missing);
    }
    Scores::open_at(&dir, &format!("{}{EXTENSION}", seed.stem()))
}

/// A list for `seed` to write, which [`Writer::finish`] renames in over the one kept. Creates
/// the folder of the lists if there is none.
///
/// # Errors
///
/// When the folder or the file can't be created.
pub fn writer(data_dir: &Path, seed: &Seed<'_>) -> Result<Writer, DatabaseError> {
    let dir = data_dir.join(DIR);
    fs::create_dir_all(&dir).map_err(|error| DatabaseError::new(NAME, &dir, error))?;
    let stem = seed.stem();
    Writer::create_at(
        dir.join(format!("{stem}{NEXT}")),
        dir.join(format!("{stem}{EXTENSION}")),
    )
}

/// Records that `seed`'s list was asked for `at`, for the sweep's order. Returns `false`,
/// recording nothing, when there is no list: a computation's sweep can delete one a reader
/// has just opened, and the reader goes on reading the open file.
///
/// # Errors
///
/// When the list's file can't be opened for another reason or its time set.
pub fn asked(data_dir: &Path, seed: &Seed<'_>, at: SystemTime) -> Result<bool, DatabaseError> {
    let path = data_dir
        .join(DIR)
        .join(format!("{}{EXTENSION}", seed.stem()));
    match File::options().write(true).open(&path) {
        Ok(file) => file
            .set_modified(at)
            .map(|()| true)
            .map_err(|error| DatabaseError::new(NAME, &path, error)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(DatabaseError::new(NAME, &path, error)),
    }
}

/// Deletes every kept list not computed from `current`, and of the rest all but the `keep`
/// most recently asked; also a file of another shape, and a `.next` a computation left. The
/// caller holds [`lock`], so no list is being written. A reader holding a list open as it is
/// deleted keeps reading it. A file that can't be opened as a list is deleted too, said on
/// `log`: the lists are a cache, and it would otherwise fail every sweep.
///
/// # Errors
///
/// When the folder can't be read, or a file can't be deleted.
pub fn sweep(
    data_dir: &Path,
    keep: usize,
    current: &Inputs,
    log: &mut dyn FnMut(&str),
) -> Result<(), DatabaseError> {
    let dir = data_dir.join(DIR);
    let fail = |path: &Path, error: std::io::Error| DatabaseError::new(NAME, path, error);
    let mut kept: Vec<(SystemTime, PathBuf)> = Vec::new();
    for entry in fs::read_dir(&dir).map_err(|error| fail(&dir, error))? {
        let entry = entry.map_err(|error| fail(&dir, error))?;
        let path = entry.path();
        // Files that aren't lists, the SQLite `-wal` and `-shm` among them, are left alone.
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if name.ends_with(NEXT) {
            sqlite::remove(&path).map_err(|error| fail(&path, error))?;
            continue;
        }
        if !name.ends_with(EXTENSION) {
            continue;
        }
        let is_current = match Scores::open_at(&dir, &name) {
            Ok(Reading::Open(scores)) => match scores.computed() {
                Ok(computed) => computed.inputs == *current,
                // A list that can't be read is no use either.
                Err(error) => {
                    log(&format!(
                        "seed list: deleting {name}, which can't be read ({error})"
                    ));
                    false
                }
            },
            Ok(Reading::Missing) => {
                log(&format!("seed list: deleting {name}, which holds no list"));
                false
            }
            Ok(Reading::OtherShape) => {
                log(&format!(
                    "seed list: deleting {name}, which is of another shape"
                ));
                false
            }
            Err(error) => {
                log(&format!(
                    "seed list: deleting {name}, which can't be read ({error})"
                ));
                false
            }
        };
        if is_current {
            let asked = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .map_err(|error| fail(&path, error))?;
            kept.push((asked, path));
        } else {
            sqlite::remove(&path).map_err(|error| fail(&path, error))?;
        }
    }
    // The most recently asked first; the name breaks a tie, so the order is the same each time.
    kept.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    for (_, path) in kept.into_iter().skip(keep) {
        sqlite::remove(&path).map_err(|error| fail(&path, error))?;
    }
    Ok(())
}
