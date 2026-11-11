//! The swap of a rebuilt file into place: the new file, written whole beside the one it
//! replaces, is synced and renamed over it, and then the directory is synced, so the rename
//! is on the disk too. A crash before the rename leaves the old file in use, and one after
//! it the new one. A reader holding the old file open keeps reading it until it opens the
//! new one.

use std::fmt;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

/// Why [`swap_in`] returned an error.
#[derive(Debug)]
pub enum SwapError {
    /// The new file couldn't be synced or renamed, so the old one stays in use. Holds the
    /// new file's path.
    Unswapped(PathBuf, io::Error),
    /// The new file is in use, but syncing its directory afterwards failed, so a crash soon
    /// after might still lose the rename. Holds the path it is in use at.
    Unsynced(PathBuf, io::Error),
}

impl SwapError {
    /// Whether the new file is in use despite this error.
    #[must_use]
    pub fn is_swapped(&self) -> bool {
        matches!(self, Self::Unsynced(..))
    }
}

impl fmt::Display for SwapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unswapped(path, error) => {
                write!(f, "{}: {error}; not swapped in", path.display())
            }
            Self::Unsynced(path, error) => write!(
                f,
                "{}: swapped in and in use, but syncing its directory failed: {error}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for SwapError {}

/// Syncs `next` and renames it over `target`, then syncs `target`'s directory.
///
/// # Errors
///
/// When the sync of `next` or the rename fails, and `target` is as it was
/// ([`SwapError::Unswapped`]); when only the sync of the directory fails, and `next` is in
/// use at `target` ([`SwapError::Unsynced`]).
pub fn swap_in(next: &Path, target: &Path) -> Result<(), SwapError> {
    swap_in_with(next, target, |dir| File::open(dir)?.sync_all())
}

/// [`swap_in`] with the sync of the directory after the rename as `sync_dir`, so a test can
/// make it fail.
///
/// # Errors
///
/// As for [`swap_in`].
pub fn swap_in_with(
    next: &Path,
    target: &Path,
    sync_dir: impl FnOnce(&Path) -> io::Result<()>,
) -> Result<(), SwapError> {
    File::open(next)
        .and_then(|file| file.sync_all())
        .and_then(|()| fs::rename(next, target))
        .map_err(|error| SwapError::Unswapped(next.to_path_buf(), error))?;
    // A bare file name's directory is the working directory.
    let dir = target
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    sync_dir(dir).map_err(|error| SwapError::Unsynced(target.to_path_buf(), error))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io;

    use super::{SwapError, swap_in, swap_in_with};
    use crate::build::tests::TempDir;

    #[test]
    fn a_swap_whose_directory_sync_fails_is_in_use_and_says_so() {
        let dir = TempDir::new("swap-unsynced");
        let (next, target) = (dir.0.join("file.next"), dir.0.join("file"));
        fs::write(&target, "old").unwrap();
        fs::write(&next, "new").unwrap();
        let mut synced = None;
        let error = swap_in_with(&next, &target, |dir| {
            synced = Some(dir.to_path_buf());
            Err(io::Error::other("disk gone"))
        })
        .unwrap_err();
        assert!(matches!(error, SwapError::Unsynced(..)), "{error:?}");
        assert!(error.is_swapped());
        assert!(error.to_string().contains("disk gone"), "{error}");
        assert_eq!(synced.as_deref(), Some(dir.0.as_path()));
        assert_eq!(fs::read_to_string(&target).unwrap(), "new");
        assert!(!next.exists());
        // A missing new file swaps nothing in, and the old file stays.
        fs::write(&target, "old").unwrap();
        let error = swap_in(&next, &target).unwrap_err();
        assert!(matches!(error, SwapError::Unswapped(..)), "{error:?}");
        assert!(!error.is_swapped());
        assert_eq!(fs::read_to_string(&target).unwrap(), "old");
    }
}
