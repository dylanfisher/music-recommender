//! The profile: the owner's library as the player describes it, in the shape
//! `docs/api.md` gives. Read from a file with `profile load` until the service's API
//! accepts it; for now only its Discogs release ids are used, queued for the sync.

use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use catalog::community::{Community, CommunityError, Source};
use config::Config;
use serde::Deserialize;

/// The largest profile accepted. The library's 6,541 albums come to about 2 MB, so this
/// leaves room for a library ten times the size and stops a wrong file early.
const MAX_BYTES: u64 = 32 * 1024 * 1024;

/// The owner's library.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// Every album in the library.
    pub albums: Vec<Album>,
}

/// One album, with the tags matching needs and the owner's listening.
#[derive(Debug, Deserialize)]
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
    Database(CommunityError),
}

impl fmt::Display for ProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(path, error) => write!(f, "{}: {error}", path.display()),
            Self::TooLarge(path) => write!(
                f,
                "{}: larger than the {} MB a profile may be",
                path.display(),
                MAX_BYTES / 1024 / 1024
            ),
            Self::Json(path, error) => write!(f, "{}: {error}", path.display()),
            Self::Invalid(path, index, problem) => {
                write!(f, "{}: albums[{index}]: {problem}", path.display())
            }
            Self::Database(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for ProfileError {}

/// Reads and checks the profile at `path`.
///
/// # Errors
///
/// When the file can't be read, is over `MAX_BYTES`, isn't a profile, or has a value out
/// of range.
pub fn load(path: &Path) -> Result<Profile, ProfileError> {
    let read = |error| ProfileError::Read(path.to_path_buf(), error);
    let mut text = String::new();
    // Reading one byte past the cap tells a file at the cap from one over it.
    File::open(path)
        .map_err(read)?
        .take(MAX_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(read)?;
    if text.len() as u64 > MAX_BYTES {
        return Err(ProfileError::TooLarge(path.to_path_buf()));
    }
    let profile: Profile = serde_json::from_str(&text)
        .map_err(|error| ProfileError::Json(path.to_path_buf(), error))?;
    for (index, album) in profile.albums.iter().enumerate() {
        let invalid = |problem| Err(ProfileError::Invalid(path.to_path_buf(), index, problem));
        if album.discogs_release_id == Some(0) {
            return invalid("discogs_release_id must be a positive number");
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
