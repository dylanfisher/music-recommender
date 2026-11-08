//! The files the downloads touch (downloads.md, Staging and the NAS; ADR 0011): where a
//! job's files land under slskd's downloads folder and in the staging root, and the one way
//! the service deletes a file. Every path is checked under its root without following a
//! symlink, and nothing is deleted that the job's ledger doesn't hold.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

/// The folder a job's files go in, under slskd's downloads folder and under the staging
/// root: `job-42`.
pub(crate) fn job_folder(id: i64) -> String {
    format!("job-{id}")
}

/// `relative` under `root`, checked one component at a time: only plain names, none of
/// them a symlink, `root` itself canonicalized first. The path must exist.
///
/// # Errors
///
/// `NotFound` when a component doesn't exist; `InvalidInput` for a symlink or a name that
/// isn't plain (`..`, an absolute path).
pub(crate) fn under(root: &Path, relative: &Path) -> io::Result<PathBuf> {
    let mut path = fs::canonicalize(root)?;
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(invalid(format!(
                "{} isn't a plain path under {}",
                relative.display(),
                root.display()
            )));
        };
        path.push(name);
        if fs::symlink_metadata(&path)?.file_type().is_symlink() {
            return Err(invalid(format!("{} is a symlink", path.display())));
        }
    }
    Ok(path)
}

/// Deletes the ledgered file at `path`, which must be under `root`, and then its folder if
/// that is left empty and isn't `root`. A file already gone is fine.
///
/// # Errors
///
/// When `path` isn't a file under `root` reached without a symlink, or the delete fails.
pub(crate) fn remove_ledgered(root: &Path, path: &str) -> io::Result<()> {
    let root = fs::canonicalize(root)?;
    let relative = Path::new(path)
        .strip_prefix(&root)
        .map_err(|_| invalid(format!("{path} isn't under {}", root.display())))?;
    let file = match under(&root, relative) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !fs::symlink_metadata(&file)?.is_file() {
        return Err(invalid(format!("{path} isn't a file")));
    }
    fs::remove_file(&file)?;
    if let Some(folder) = file.parent().filter(|folder| *folder != root) {
        remove_if_empty(folder)?;
    }
    Ok(())
}

/// Removes `folder` if it is empty.
fn remove_if_empty(folder: &Path) -> io::Result<()> {
    match fs::remove_dir(folder) {
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::DirectoryNotEmpty | io::ErrorKind::NotFound
            ) =>
        {
            Ok(())
        }
        other => other,
    }
}

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// The bytes free to this user on the volume holding `path`, as `df -Pk` reports them.
/// `std` has no `statvfs`, and reading `df` needs no new crate.
///
/// # Errors
///
/// When `df` can't run, fails, or prints something other than its POSIX table.
pub fn free_space(path: &Path) -> io::Result<u64> {
    let output = Command::new("/bin/df").arg("-Pk").arg(path).output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "df {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .nth(1)
        .and_then(|line| line.split_whitespace().nth(3))
        .and_then(|kilobytes| kilobytes.parse::<u64>().ok())
        .map(|kilobytes| kilobytes * 1024)
        .ok_or_else(|| io::Error::other(format!("df printed {text:?}")))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::{Path, PathBuf};

    use super::{free_space, remove_ledgered, under};

    fn dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("music-recommender-staging")
            .join(test);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::canonicalize(dir).unwrap()
    }

    #[test]
    fn under_refuses_a_symlink_and_a_name_that_climbs_out() {
        let root = dir("under");
        fs::create_dir_all(root.join("a")).unwrap();
        fs::write(root.join("a/f"), "x").unwrap();
        symlink(root.join("a"), root.join("link")).unwrap();
        assert_eq!(under(&root, Path::new("a/f")).unwrap(), root.join("a/f"));
        assert!(under(&root, Path::new("link/f")).is_err());
        assert!(under(&root, Path::new("../under/a/f")).is_err());
        assert!(under(&root, Path::new("/etc/hosts")).is_err());
    }

    #[test]
    fn removing_a_ledgered_file_leaves_its_neighbor_and_the_root() {
        let root = dir("remove");
        fs::create_dir_all(root.join("job-1")).unwrap();
        fs::write(root.join("job-1/a.flac"), "a").unwrap();
        fs::write(root.join("job-1/b.flac"), "b").unwrap();
        fs::write(root.join("owner.flac"), "o").unwrap();
        let ledgered = |name: &str| root.join(name).to_str().unwrap().to_string();
        remove_ledgered(&root, &ledgered("job-1/a.flac")).unwrap();
        assert!(root.join("job-1/b.flac").exists());
        remove_ledgered(&root, &ledgered("job-1/b.flac")).unwrap();
        assert!(!root.join("job-1").exists());
        remove_ledgered(&root, &ledgered("job-1/b.flac")).unwrap();
        assert!(root.join("owner.flac").exists());
        assert!(remove_ledgered(&root, "/etc/hosts").is_err());
    }

    #[test]
    fn free_space_reads_df() {
        assert!(free_space(&std::env::temp_dir()).unwrap() > 0);
    }
}
