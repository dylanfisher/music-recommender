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

/// The folder under the staging root a copy is made in before it is renamed into place, so
/// a job's folder is either whole or absent.
const INCOMING: &str = ".incoming";

/// The longest name a component may have, in bytes, as APFS and SMB allow.
const MAX_NAME_BYTES: usize = 255;

/// A file to stage: where slskd put it, its path under the job's folder as the peer named
/// it (`/`-separated), and the size it must have.
pub(crate) struct Source {
    pub(crate) path: PathBuf,
    pub(crate) name: String,
    pub(crate) size: u64,
}

/// A peer's name as one safe path component: `/`, `:` and `\` replaced, control
/// characters dropped, no leading dot or space (so no `.`, `..` or hidden file), at most 255 bytes
/// with the extension kept, and `_` for a name left empty.
pub(crate) fn component(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| {
            if matches!(c, '/' | ':' | '\\') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned
        .trim_start_matches(|c: char| c == '.' || c.is_whitespace())
        .trim_end();
    if cleaned.is_empty() {
        return "_".to_string();
    }
    if cleaned.len() <= MAX_NAME_BYTES {
        return cleaned.to_string();
    }
    let extension = cleaned
        .rfind('.')
        .map_or("", |dot| &cleaned[dot..])
        .chars()
        .take(16)
        .collect::<String>();
    let mut stem_end = MAX_NAME_BYTES - extension.len();
    while !cleaned.is_char_boundary(stem_end) {
        stem_end -= 1;
    }
    format!("{}{extension}", &cleaned[..stem_end])
}

/// A peer's `/`-separated path as safe components.
pub(crate) fn safe_path(name: &str) -> String {
    name.split('/')
        .filter(|part| !part.is_empty())
        .map(component)
        .collect::<Vec<_>>()
        .join("/")
}

/// Removes what a stopped copy left in `.incoming`. The staging root not being there (the
/// share not mounted) leaves nothing to remove.
///
/// # Errors
///
/// When `.incoming` is there and can't be removed.
pub(crate) fn clear_incoming(root: &Path) -> io::Result<()> {
    match under(root, Path::new(INCOMING)) {
        Ok(incoming) => remove_tree(&incoming),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Removes a folder the service made, with what is in it. `remove_dir_all` deletes a
/// symlink inside it, never what the link points at.
fn remove_tree(folder: &Path) -> io::Result<()> {
    if !fs::symlink_metadata(folder)?.is_dir() {
        return Err(invalid(format!("{} isn't a folder", folder.display())));
    }
    fs::remove_dir_all(folder)
}

/// Copies `files` into `folder` under the staging root `root`: each checked at its source,
/// copied into `.incoming/<folder>`, checked again, and the whole folder renamed into place.
/// A `folder` already there is from a copy stopped after its rename and before the store
/// recorded it, and is made again. Answers each file's path under `folder`, or the reason
/// the copy can't be made, for the job's line.
pub(crate) fn stage(root: &Path, folder: &str, files: &[Source]) -> Result<Vec<String>, String> {
    let root = fs::canonicalize(root).map_err(|error| {
        format!(
            "the staging folder {} isn't there ({error}); is the NAS mounted?",
            root.display()
        )
    })?;
    let words = |error: io::Error| error.to_string();
    let incoming = root.join(INCOMING);
    if !incoming.exists() {
        fs::create_dir(&incoming).map_err(words)?;
    }
    let incoming = under(&root, Path::new(INCOMING)).map_err(words)?;
    let copy = incoming.join(folder);
    if fs::symlink_metadata(&copy).is_ok() {
        remove_tree(&copy).map_err(words)?;
    }
    let names = match copy_all(&copy, files) {
        Ok(names) => names,
        Err(reason) => {
            return Err(match remove_tree(&copy) {
                Ok(()) => reason,
                Err(error) => format!("{reason}; and the partial copy stays: {error}"),
            });
        }
    };
    let placed = root.join(folder);
    if fs::symlink_metadata(&placed).is_ok() {
        remove_tree(&placed).map_err(words)?;
    }
    fs::rename(&copy, &placed).map_err(words)?;
    Ok(names)
}

/// Copies each file into `copy`, checking its size before and after, and answers their
/// paths under it.
fn copy_all(copy: &Path, files: &[Source]) -> Result<Vec<String>, String> {
    let words = |error: io::Error| error.to_string();
    let mut names = Vec::with_capacity(files.len());
    for file in files {
        let size = fs::symlink_metadata(&file.path).map_err(words)?.len();
        if size != file.size {
            return Err(format!("{} is {size} bytes, not {}", file.name, file.size));
        }
        let name = safe_path(&file.name);
        if names.contains(&name) {
            return Err(format!("two files are named {name}"));
        }
        let destination = copy.join(&name);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(words)?;
        }
        fs::copy(&file.path, &destination).map_err(words)?;
        let copied = fs::symlink_metadata(&destination).map_err(words)?.len();
        if copied != file.size {
            return Err(format!(
                "{name} copied as {copied} bytes, not {}",
                file.size
            ));
        }
        names.push(name);
    }
    Ok(names)
}

/// Deletes a staged job's `files`, the paths under `folder` its ledger holds, then each
/// folder left empty, and answers whether `folder` is gone. `delete_files` false only
/// removes folders left empty, as after an import moved the files out. Anything not in the
/// ledger stays, and so does its folder.
///
/// # Errors
///
/// When a path isn't under the staging root without a symlink, or a delete fails.
pub(crate) fn remove_staged(
    root: &Path,
    folder: &str,
    files: &[String],
    delete_files: bool,
) -> io::Result<bool> {
    let root = fs::canonicalize(root)?;
    let job = root.join(component(folder));
    if fs::symlink_metadata(&job).is_err() {
        return Ok(true);
    }
    let job = under(&root, Path::new(&component(folder)))?;
    let mut folders = Vec::new();
    for name in files {
        let path = job.join(name);
        if delete_files {
            remove_ledgered(&root, &path.display().to_string())?;
        }
        folders.extend(
            path.ancestors()
                .skip(1)
                .take_while(|f| *f != job)
                .map(Path::to_path_buf),
        );
    }
    // Deepest first, so a disc folder goes before the job's.
    folders.sort_by_key(|folder| std::cmp::Reverse(folder.components().count()));
    folders.dedup();
    for folder in folders {
        remove_if_empty(&folder)?;
    }
    remove_if_empty(&job)?;
    Ok(fs::symlink_metadata(&job).is_err())
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

    use super::{component, free_space, remove_ledgered, safe_path, under};

    #[test]
    fn a_hostile_name_becomes_one_plain_component() {
        assert_eq!(component("01 - First: Song.flac"), "01 - First_ Song.flac");
        assert_eq!(component("a/b\\c"), "a_b_c");
        assert_eq!(component(".."), "_");
        for name in [" ..", ". ..", ".\u{a0}.", "\t.", " . "] {
            assert_eq!(component(name), "_", "{name:?}");
        }
        assert_eq!(component(".hidden.flac"), "hidden.flac");
        assert_eq!(component("bell\u{7}.flac"), "bell.flac");
        assert_eq!(component(""), "_");
        let long = format!("{}.flac", "é".repeat(200));
        let short = component(&long);
        assert!(short.len() <= 255, "{short}");
        assert_eq!(&short[short.len() - 5..], ".flac");
        assert_eq!(safe_path("CD1/../1-01 x.flac"), "CD1/_/1-01 x.flac");
        assert_eq!(safe_path("/abs//01.flac"), "abs/01.flac");
    }

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
