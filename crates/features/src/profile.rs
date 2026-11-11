//! The profile: the owner's library as the player describes it, in the shape
//! `docs/api.md` gives. The player sends it to `POST /v1/profile`, and `profile load` reads
//! the same JSON from a file. The last one accepted is kept in the data directory, replacing
//! the one before it whole, and its Discogs release ids are queued for the sync (ADR 0025).

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use catalog::community::{Community, Source};
use catalog::error::DatabaseError;
use config::Config;
use serde::{Deserialize, Serialize};

/// The largest profile accepted. The library's 6,541 albums come to about 2 MB, so this
/// leaves room for a library ten times the size and stops a wrong file early.
pub const MAX_BYTES: usize = 32 * 1024 * 1024;

/// The kept profile, in the data directory.
const FILE_NAME: &str = "profile.json";
/// Gives each profile being kept its own file name, so two keeps at once, in one process
/// or two, don't write the same file.
static KEEPS: AtomicU64 = AtomicU64::new(0);
/// How long a `profile.next.*.json` can sit before the keep that made it counts as dead. A
/// keep writes a file of a few megabytes and renames it at once, so a file this old was left
/// by a crash between the create and the rename.
const STALE_AFTER: Duration = Duration::from_mins(1);

/// The owner's library.
#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// Every album in the library.
    pub albums: Vec<Album>,
}

/// One album, with the tags matching needs and the owner's listening.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Album {
    /// The album artist.
    pub artist: String,
    /// The album title.
    pub title: String,
    /// The number of tracks.
    pub track_count: u32,
    /// Each track's length in seconds, in track order.
    pub durations: Vec<u32>,
    /// Catalog numbers from the tags, possibly none.
    pub catalog_numbers: Vec<String>,
    /// Barcodes from the tags, possibly none.
    pub barcodes: Vec<String>,
    /// The Discogs release id, when the album is tagged with one.
    #[serde(default)]
    pub discogs_release_id: Option<u64>,
    /// Plays of the album's tracks, summed.
    pub plays: u64,
    /// How many of its tracks are loved.
    pub loved: u32,
    /// The owner's rating from 1 to 5, absent when unrated.
    #[serde(default)]
    pub rating: Option<u8>,
}

/// Why a profile was refused. Each names the file.
#[derive(Debug)]
pub enum ProfileError {
    /// The file can't be read.
    Read(PathBuf, io::Error),
    /// The file is larger than `MAX_BYTES`.
    TooLarge(PathBuf),
    /// The file isn't JSON in the profile's shape.
    Json(PathBuf, serde_json::Error),
    /// An album has a value out of range. Holds its position in `albums`, from 0.
    Invalid(PathBuf, usize, &'static str),
    /// The community database failed.
    Database(DatabaseError),
    /// The kept profile can't be written.
    Write(PathBuf, io::Error),
    /// The profile is kept, but syncing its directory afterwards failed, so a crash soon
    /// after might still lose it.
    Unsynced(PathBuf, io::Error),
    /// The kept profile isn't a profile. Reading leaves the file; a keep replaces it. Holds
    /// why it was refused.
    Unparsed(Box<ProfileError>),
}

impl fmt::Display for ProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(path, error) | Self::Write(path, error) => {
                write!(f, "{}: {error}", path.display())
            }
            Self::TooLarge(path) => write!(
                f,
                "{}: larger than the {} MB a profile may be",
                path.display(),
                MAX_BYTES / 1024 / 1024
            ),
            Self::Unsynced(path, error) => write!(
                f,
                "{}: the profile is kept, but syncing its directory failed: {error}",
                path.display()
            ),
            Self::Json(path, error) => write!(f, "{}: {error}", path.display()),
            Self::Invalid(path, index, problem) => {
                write!(f, "{}: albums[{index}]: {problem}", path.display())
            }
            Self::Database(error) => error.fmt(f),
            Self::Unparsed(refused) => write!(
                f,
                "kept profile {refused}; a new `profile load` or POST /v1/profile replaces it"
            ),
        }
    }
}

impl std::error::Error for ProfileError {}

impl ProfileError {
    /// Whether the profile is in place despite this error, which `keep` returns when only
    /// the sync after its rename failed.
    #[must_use]
    pub fn is_kept(&self) -> bool {
        matches!(self, Self::Unsynced(..))
    }
}

/// Why a profile's JSON was refused.
#[derive(Debug)]
pub enum Refused {
    /// It isn't JSON in the profile's shape.
    Json(serde_json::Error),
    /// An album has a value out of range. Holds its position in `albums`, from 0.
    Invalid(usize, &'static str),
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => error.fmt(f),
            Self::Invalid(index, problem) => write!(f, "albums[{index}]: {problem}"),
        }
    }
}

/// Reads and checks the profile at `path`.
///
/// # Errors
///
/// When the file can't be read, is over `MAX_BYTES`, isn't a profile, or has a value out
/// of range.
pub fn load(path: &Path) -> Result<Profile, ProfileError> {
    read_file(path, Some(MAX_BYTES))
}

/// Reads and checks the profile at `path`, refusing one over `cap` bytes when there is a
/// cap.
fn read_file(path: &Path, cap: Option<usize>) -> Result<Profile, ProfileError> {
    let read = |error| ProfileError::Read(path.to_path_buf(), error);
    let mut text = Vec::new();
    // Reading one byte past the cap tells a file at the cap from one over it.
    let limit = cap.map_or(u64::MAX, |cap| cap as u64 + 1);
    File::open(path)
        .map_err(read)?
        .take(limit)
        .read_to_end(&mut text)
        .map_err(read)?;
    if cap.is_some_and(|cap| text.len() > cap) {
        return Err(ProfileError::TooLarge(path.to_path_buf()));
    }
    parse(&text).map_err(|refused| match refused {
        Refused::Json(error) => ProfileError::Json(path.to_path_buf(), error),
        Refused::Invalid(index, problem) => {
            ProfileError::Invalid(path.to_path_buf(), index, problem)
        }
    })
}

/// Reads and checks a profile's JSON, as `POST /v1/profile` takes it. The caller caps its
/// size at `MAX_BYTES`.
///
/// # Errors
///
/// When it isn't a profile, or has a value out of range.
pub fn parse(json: &[u8]) -> Result<Profile, Refused> {
    let profile: Profile = serde_json::from_slice(json).map_err(Refused::Json)?;
    for (index, album) in profile.albums.iter().enumerate() {
        let invalid = |problem| Err(Refused::Invalid(index, problem));
        // The community database stores ids as SQLite integers, which stop at i64::MAX.
        if album
            .discogs_release_id
            .is_some_and(|id| !(1..=i64::MAX.cast_unsigned()).contains(&id))
        {
            return invalid("discogs_release_id must be from 1 to 9223372036854775807");
        }
        if album
            .rating
            .is_some_and(|rating| !(1..=5).contains(&rating))
        {
            return invalid("rating must be from 1 to 5");
        }
    }
    Ok(profile)
}

/// Keeps `profile` in `data_dir`, replacing the one kept before it whole. It is written to
/// a file of its own beside the kept file and renamed over it once synced, so a crash
/// leaves one or the other, and keeps at once leave the last one renamed. A file that an
/// earlier keep left by a crash, one over `STALE_AFTER` old, is removed first.
///
/// # Errors
///
/// When the directory can't be listed or a file can't be written or renamed: nothing is
/// kept then. When only the sync of
/// the directory after the rename fails, the profile is kept and the error says so
/// ([`ProfileError::is_kept`]).
pub fn keep(profile: &Profile, data_dir: &Path) -> Result<(), ProfileError> {
    keep_with(profile, data_dir, |dir| File::open(dir)?.sync_all())
}

/// [`keep`] with the sync of the directory after the rename as `sync_dir`.
fn keep_with(
    profile: &Profile,
    data_dir: &Path,
    sync_dir: impl FnOnce(&Path) -> io::Result<()>,
) -> Result<(), ProfileError> {
    remove_stale(data_dir)?;
    let next = data_dir.join(format!(
        "profile.next.{}.{}.json",
        std::process::id(),
        KEEPS.fetch_add(1, Ordering::Relaxed)
    ));
    let json =
        serde_json::to_vec(profile).map_err(|error| ProfileError::Json(next.clone(), error))?;
    let write = |error| ProfileError::Write(next.clone(), error);
    let written = File::create(&next).and_then(|mut file| {
        file.write_all(&json)?;
        file.sync_all()?;
        fs::rename(&next, data_dir.join(FILE_NAME))
    });
    if let Err(error) = written {
        // A partly written file would stay for good under its own name. Removing it can
        // fail too, and the write's error is the one to report.
        let _ = fs::remove_file(&next);
        return Err(write(error));
    }
    sync_dir(data_dir).map_err(|error| ProfileError::Unsynced(data_dir.join(FILE_NAME), error))
}

/// Removes the `profile.next.*.json` files in `data_dir` that are over `STALE_AFTER` old.
/// Younger ones may be another keep's, writing now, so they stay; there is no way to ask
/// whether another process is alive without a new dependency. A file that another keep
/// removed first is gone all the same.
fn remove_stale(data_dir: &Path) -> Result<(), ProfileError> {
    let write = |error| ProfileError::Write(data_dir.to_path_buf(), error);
    for entry in fs::read_dir(data_dir).map_err(write)? {
        let entry = entry.map_err(write)?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let is_next = name.starts_with("profile.next.")
            && Path::new(name)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("json"));
        if !is_next {
            continue;
        }
        let age = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .map(|modified| {
                SystemTime::now()
                    .duration_since(modified)
                    .unwrap_or_default()
            });
        match age {
            Ok(age) if age > STALE_AFTER => match fs::remove_file(entry.path()) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => {
                    return Err(ProfileError::Write(entry.path(), error));
                }
                _ => {}
            },
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(ProfileError::Write(entry.path(), error)),
        }
    }
    Ok(())
}

/// The profile kept in `data_dir`, or `None` when none has been kept.
///
/// # Errors
///
/// When the file can't be read, or isn't a profile: the file stays, since several read-only
/// paths call this, and the error says that a new `profile load` or POST replaces it. The
/// request cap doesn't apply: `keep` writes the absent id and rating as `null`, so what it
/// wrote can be longer than the request it came from.
pub fn kept(data_dir: &Path) -> Result<Option<Profile>, ProfileError> {
    let path = data_dir.join(FILE_NAME);
    match read_file(&path, None) {
        Ok(profile) => Ok(Some(profile)),
        Err(ProfileError::Read(_, error)) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(refused @ (ProfileError::Json(..) | ProfileError::Invalid(..))) => {
            Err(ProfileError::Unparsed(Box::new(refused)))
        }
        Err(error) => Err(error),
    }
}

/// What queueing a profile did.
#[derive(Debug, PartialEq, Eq)]
pub struct Queued {
    /// Albums in the profile.
    pub albums: usize,
    /// Albums with a Discogs release id.
    pub with_id: usize,
    /// Release ids that weren't queued before.
    pub added: u64,
}

impl fmt::Display for Queued {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} albums, {} with a Discogs release id; queued {} new",
            self.albums, self.with_id, self.added
        )
    }
}

/// Queues the profile's release ids for the sync, as the library's, in profile order.
///
/// # Errors
///
/// When the community database fails.
pub fn queue(profile: &Profile, config: &Config) -> Result<Queued, ProfileError> {
    let ids: Vec<u64> = profile
        .albums
        .iter()
        .filter_map(|album| album.discogs_release_id)
        .collect();
    let added = Community::open(&config.data_dir)
        .and_then(|mut db| db.queue(&ids, Source::Library))
        .map_err(ProfileError::Database)?;
    Ok(Queued {
        albums: profile.albums.len(),
        with_id: ids.len(),
        added,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io;

    use super::{FILE_NAME, Profile, ProfileError, keep_with, kept};

    #[test]
    fn a_profile_whose_directory_sync_fails_is_kept_and_says_so() {
        let dir = std::env::temp_dir()
            .join("music-recommender-profile")
            .join("unsynced");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let profile = Profile { albums: Vec::new() };
        let error = keep_with(&profile, &dir, |_| Err(io::Error::other("disk gone"))).unwrap_err();
        assert!(matches!(error, ProfileError::Unsynced(..)), "{error:?}");
        assert!(error.is_kept());
        assert!(error.to_string().contains("is kept"), "{error}");
        assert!(error.to_string().contains("disk gone"), "{error}");
        // The rename happened: the profile is in place, and no half-written file is left.
        assert!(dir.join(FILE_NAME).exists());
        assert!(kept(&dir).unwrap().is_some());
        let leftovers: Vec<_> = fs::read_dir(&dir).unwrap().collect();
        assert_eq!(leftovers.len(), 1, "{leftovers:?}");
        // A failed write keeps nothing, and isn't `is_kept`.
        let missing = dir.join("no-such-directory");
        let error = keep_with(&profile, &missing, |_| Ok(())).unwrap_err();
        assert!(matches!(error, ProfileError::Write(..)), "{error:?}");
        assert!(!error.is_kept());
    }
}
