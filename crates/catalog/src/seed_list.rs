//! The seed lists kept on disk (ADR 0028): a seed album's or a seed artist's list, computed
//! when it is first asked for and kept in `seed-lists/` in the data directory, one file a
//! seed, of `scores.sqlite`'s shape with the `computed` row it was scored from, so later
//! pages and a `serve` restarted read it instead of a pass over the catalog.
//!
//! Each list is written as a `.next` file and renamed in whole, as the scores are. One
//! computation runs at a time, under `seed-lists.lock`, and the sweep after it deletes every
//! list whose inputs aren't the current ones and the least recently asked past a cap. When
//! a list was last asked for is its file's modification time, which [`asked`] sets.

use std::fmt;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::community::{LockError, SyncLock, lock_waiting};
use crate::error::DatabaseError;
use crate::score::{Computed, Inputs, Scores, Writer, digest};
use crate::sqlite::{self, Reading};

/// The folder of the lists, inside the data directory.
const DIR: &str = "seed-lists";

/// A list's file ends with this.
const EXTENSION: &str = ".sqlite";

/// A list being written ends with this, and is renamed to the list's file when complete.
const NEXT: &str = ".next.sqlite";

/// The lock file a computation holds, so two can't run at once in any process.
const LOCK_FILE: &str = "seed-lists.lock";

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

    /// The list's file name, as the sweep names it on the log.
    #[must_use]
    pub fn file_name(&self) -> String {
        format!("{}{EXTENSION}", self.stem())
    }
}

/// Takes the lock a computation of a seed list holds, waiting while another holds it, in
/// this process or another; `waiting` is called once before that wait.
///
/// # Errors
///
/// When a lock file can't be opened or locked.
pub fn lock(data_dir: &Path, waiting: impl FnOnce()) -> Result<SyncLock, LockError> {
    lock_waiting(data_dir.join(LOCK_FILE), waiting)
}

/// What a kept list's file holds, by how a reader goes on.
pub enum Kept {
    /// A list that opens, with what it was computed from.
    Usable(Scores, Computed),
    /// A file that is no list to read, which the next computation replaces.
    Unusable(Unusable),
    /// A file that can't be opened now for a reason that says nothing about its contents (no
    /// file handle, a lock, a permission), which may open later: deleting it would be a
    /// guess.
    Failed(DatabaseError),
}

/// Why a file is no list to read.
#[derive(Debug)]
pub enum Unusable {
    /// There is no file.
    Missing,
    /// A file another version wrote.
    OtherShape,
    /// A file that is no database or is corrupt, or whose `computed` row can't be read.
    Damaged(DatabaseError),
}

impl fmt::Display for Unusable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => write!(f, "holds no list"),
            Self::OtherShape => write!(f, "is of another shape"),
            Self::Damaged(error) => write!(f, "can't be read ({error})"),
        }
    }
}

/// `seed`'s list, opened to read.
#[must_use]
pub fn open(data_dir: &Path, seed: &Seed<'_>) -> Kept {
    let dir = data_dir.join(DIR);
    if !dir.is_dir() {
        return Kept::Unusable(Unusable::Missing);
    }
    classify(&dir, &seed.file_name())
}

/// Each kept list's file name with when it was last asked for, in no order; none when there
/// is no folder. A list being written isn't one.
///
/// # Errors
///
/// When the folder or a file's time can't be read.
pub fn asked_lists(data_dir: &Path) -> Result<Vec<(SystemTime, String)>, DatabaseError> {
    let dir = data_dir.join(DIR);
    let fail = |path: &Path, error: std::io::Error| DatabaseError::new(NAME, path, error);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(fail(&dir, error)),
    };
    let mut lists = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| fail(&dir, error))?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if name.ends_with(NEXT) || !name.ends_with(EXTENSION) {
            continue;
        }
        let asked = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .map_err(|error| fail(&entry.path(), error))?;
        lists.push((asked, name));
    }
    Ok(lists)
}

/// The kept list in the file `name` of the lists' folder, as [`asked_lists`] names it,
/// opened to read.
#[must_use]
pub fn open_named(data_dir: &Path, name: &str) -> Kept {
    classify(&data_dir.join(DIR), name)
}

/// The file `name` in the lists' folder `dir`, opened to read.
fn classify(dir: &Path, name: &str) -> Kept {
    match Scores::open_at(dir, name) {
        Ok(Reading::Open(scores)) => match scores.computed() {
            Ok(computed) => Kept::Usable(scores, computed),
            Err(error) => Kept::Unusable(Unusable::Damaged(error)),
        },
        Ok(Reading::Missing) => Kept::Unusable(Unusable::Missing),
        Ok(Reading::OtherShape) => Kept::Unusable(Unusable::OtherShape),
        Err(error) if error.is_damaged() => Kept::Unusable(Unusable::Damaged(error)),
        Err(error) => Kept::Failed(error),
    }
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
/// deleted keeps reading it. A file that is no database or is corrupt is deleted too, said on
/// `log`: the lists are a cache, and it would otherwise fail every sweep. One that can't be
/// opened for another reason (no file handle, a lock, a permission) is left and said.
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
        let is_current = match classify(&dir, &name) {
            Kept::Usable(_, computed) => computed.inputs == *current,
            // A list that can't be read is no use either.
            Kept::Unusable(why) => {
                log(&format!("seed list: deleting {name}, which {why}"));
                false
            }
            // Nothing says the file is bad: it may open at the next sweep.
            Kept::Failed(error) => {
                log(&format!(
                    "seed list: skipping {name}, which can't be opened now ({error})"
                ));
                continue;
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
