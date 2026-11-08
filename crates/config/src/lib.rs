//! Reads the service's configuration, the one place that does, and fails on a missing key.
//!
//! The file is `~/Library/Application Support/music-recommender/config.toml`, owner-only
//! (mode 0600), with these keys:
//!
//! | Key | Required | What it is |
//! |---|---|---|
//! | `discogs_token` | yes | The owner's Discogs personal access token. |
//! | `player_token` | yes | The token the player sends to the service's API. |
//! | `data_dir` | yes | An absolute path to an existing directory: the community database and the sync's lock file live there. |
//! | `[slskd]` `url` | with `[staging]` | slskd's base URL, `http://` or `https://`. |
//! | `[slskd]` `api_key` | with `[staging]` | A `readwrite` slskd API key. |
//! | `[slskd]` `downloads_dir` | with `[staging]` | An absolute path: the folder slskd writes finished downloads to. |
//! | `[staging]` `dir` | with `[slskd]` | An absolute path: the staging root on the NAS. |
//!
//! The two tables turn on the Soulseek downloads (docs/downloads.md, ADR 0007) and come
//! together or not at all. Any other key is refused, so a misspelled key fails instead of
//! being ignored.

use std::fmt;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// The keys this file accepts.
const DISCOGS_TOKEN: &str = "discogs_token";
const PLAYER_TOKEN: &str = "player_token";
const DATA_DIR: &str = "data_dir";
const SLSKD: &str = "slskd";
const SLSKD_URL: &str = "slskd.url";
const SLSKD_API_KEY: &str = "slskd.api_key";
const SLSKD_DOWNLOADS_DIR: &str = "slskd.downloads_dir";
const STAGING: &str = "staging";
const STAGING_DIR: &str = "staging.dir";

/// Permission bits for group and others. Any of them set refuses the file: it holds a
/// token, and a file others can write could have its token or data directory swapped.
const GROUP_OR_OTHER: u32 = 0o077;

/// The service's configuration, read from one file.
#[derive(Debug)]
pub struct Config {
    /// The file this was read from, for error messages that tell the owner where to look.
    pub path: PathBuf,
    /// `discogs_token`.
    pub discogs_token: Secret,
    /// `player_token`: the bearer token every API request must carry.
    pub player_token: Secret,
    /// `data_dir`: where the community database and the sync's lock file live.
    pub data_dir: PathBuf,
    /// The `[slskd]` and `[staging]` tables, or `None` when the downloads are off.
    pub downloads: Option<Downloads>,
}

/// The Soulseek downloads' settings, from the `[slskd]` and `[staging]` tables.
#[derive(Debug)]
pub struct Downloads {
    /// `slskd.url`: slskd's base URL.
    pub slskd_url: String,
    /// `slskd.api_key`, sent as `X-API-Key`.
    pub slskd_api_key: Secret,
    /// `slskd.downloads_dir`: where slskd writes finished downloads on this Mac.
    pub slskd_downloads_dir: PathBuf,
    /// `staging.dir`: the staging root on the NAS, as this Mac mounts it.
    pub staging_dir: PathBuf,
}

/// A token or key from the file. `Debug` prints `***` and there is no `Display`, so a
/// secret can't end up in a log line or an error message by accident; `secret` is the one
/// way to read it, for the header it goes in.
pub struct Secret(String);

impl Secret {
    /// The secret itself.
    #[must_use]
    pub fn secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

/// Why the configuration couldn't be read. Each names the file, and the key where one is
/// at fault. None of them carries a value from the file, so none can carry the token.
#[derive(Debug)]
#[expect(missing_docs, reason = "the fields are named for what they hold")]
pub enum ConfigError {
    /// `HOME` isn't set, so there is no default path.
    NoHome,
    /// The file can't be read: missing, or not ours to read.
    Read { path: PathBuf, error: io::Error },
    /// Group or others have a permission bit on the file.
    Permissions { path: PathBuf, mode: u32 },
    /// The file isn't TOML.
    Syntax {
        path: PathBuf,
        line: usize,
        message: String,
    },
    /// A key this crate doesn't know, often a misspelling.
    UnknownKey { path: PathBuf, key: String },
    /// A required key is absent.
    MissingKey { path: PathBuf, key: &'static str },
    /// A key's value isn't a string.
    NotAString { path: PathBuf, key: &'static str },
    /// A key that names a table, such as `slskd`, holds something else.
    NotATable { path: PathBuf, key: &'static str },
    /// One of the downloads' two tables is there without the other.
    HalfConfigured {
        path: PathBuf,
        present: &'static str,
        missing: &'static str,
    },
    /// A URL that doesn't start with `http://` or `https://`.
    NotAUrl { path: PathBuf, key: &'static str },
    /// A key's value is the empty string.
    Empty { path: PathBuf, key: &'static str },
    /// A path is relative, which would depend on where the service started.
    NotAbsolute { path: PathBuf, key: &'static str },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoHome => write!(f, "HOME is not set, so the config file can't be found"),
            Self::Read { path, error } => write!(f, "{}: {error}", path.display()),
            Self::Permissions { path, mode } => write!(
                f,
                "{}: mode is {mode:04o}, but it holds a token and must be readable and \
                 writable by its owner only (chmod 600)",
                path.display()
            ),
            Self::Syntax {
                path,
                line,
                message,
            } => write!(f, "{}:{line}: not valid TOML: {message}", path.display()),
            Self::UnknownKey { path, key } => write!(f, "{}: unknown key `{key}`", path.display()),
            Self::MissingKey { path, key } => {
                write!(f, "{}: missing key `{key}`", path.display())
            }
            Self::NotAString { path, key } => {
                write!(f, "{}: `{key}` must be a string", path.display())
            }
            Self::NotATable { path, key } => {
                write!(f, "{}: `{key}` must be a table, `[{key}]`", path.display())
            }
            Self::HalfConfigured {
                path,
                present,
                missing,
            } => write!(
                f,
                "{}: `[{present}]` is set but `[{missing}]` is missing; the downloads need \
                 both, or neither to stay off",
                path.display()
            ),
            Self::NotAUrl { path, key } => write!(
                f,
                "{}: `{key}` must start with http:// or https://",
                path.display()
            ),
            Self::Empty { path, key } => write!(f, "{}: `{key}` is empty", path.display()),
            Self::NotAbsolute { path, key } => {
                write!(f, "{}: `{key}` must be an absolute path", path.display())
            }
        }
    }
}

impl std::error::Error for ConfigError {}

/// `~/Library/Application Support/music-recommender/config.toml`.
///
/// # Errors
///
/// When `HOME` isn't set.
pub fn default_path() -> Result<PathBuf, ConfigError> {
    let home = std::env::var_os("HOME").ok_or(ConfigError::NoHome)?;
    Ok(PathBuf::from(home).join("Library/Application Support/music-recommender/config.toml"))
}

/// Reads and checks the config file at `path`.
///
/// # Errors
///
/// When the file can't be read, is readable or writable by group or others, isn't TOML,
/// has a key it doesn't know, lacks a required key, or has a value of the wrong shape.
pub fn load(path: &Path) -> Result<Config, ConfigError> {
    let read = |error| ConfigError::Read {
        path: path.to_path_buf(),
        error,
    };
    let mode = fs::metadata(path).map_err(read)?.permissions().mode();
    if mode & GROUP_OR_OTHER != 0 {
        return Err(ConfigError::Permissions {
            path: path.to_path_buf(),
            mode: mode & 0o7777,
        });
    }
    let text = fs::read_to_string(path).map_err(read)?;
    // toml's `Display` quotes the offending line, which could be the token's; its
    // `message` doesn't, so only that and a line number are kept.
    let mut table: toml::Table = text.parse().map_err(|error: toml::de::Error| {
        let line = error
            .span()
            .map_or(1, |span| text[..span.start].matches('\n').count() + 1);
        ConfigError::Syntax {
            path: path.to_path_buf(),
            line,
            message: error.message().to_string(),
        }
    })?;
    let file = File { path };
    let slskd = file.take_table(&mut table, SLSKD)?;
    let staging = file.take_table(&mut table, STAGING)?;
    let discogs_token = file.take(&mut table, DISCOGS_TOKEN)?;
    let player_token = file.take(&mut table, PLAYER_TOKEN)?;
    let data_dir = file.take(&mut table, DATA_DIR)?;
    file.no_more(&table, "")?;
    let discogs_token = Secret(file.required(discogs_token, DISCOGS_TOKEN)?);
    let player_token = Secret(file.required(player_token, PLAYER_TOKEN)?);
    let data_dir = file.absolute(file.required(data_dir, DATA_DIR)?, DATA_DIR)?;
    let downloads = match (slskd, staging) {
        (None, None) => None,
        (Some(_), None) => return Err(file.half(SLSKD, STAGING)),
        (None, Some(_)) => return Err(file.half(STAGING, SLSKD)),
        (Some(mut slskd), Some(mut staging)) => {
            let url = file.take(&mut slskd, SLSKD_URL)?;
            let api_key = file.take(&mut slskd, SLSKD_API_KEY)?;
            let downloads_dir = file.take(&mut slskd, SLSKD_DOWNLOADS_DIR)?;
            file.no_more(&slskd, SLSKD)?;
            let staging_dir = file.take(&mut staging, STAGING_DIR)?;
            file.no_more(&staging, STAGING)?;
            let url = file.required(url, SLSKD_URL)?;
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return Err(ConfigError::NotAUrl {
                    path: path.to_path_buf(),
                    key: SLSKD_URL,
                });
            }
            Some(Downloads {
                slskd_url: url,
                slskd_api_key: Secret(file.required(api_key, SLSKD_API_KEY)?),
                slskd_downloads_dir: file.absolute(
                    file.required(downloads_dir, SLSKD_DOWNLOADS_DIR)?,
                    SLSKD_DOWNLOADS_DIR,
                )?,
                staging_dir: file
                    .absolute(file.required(staging_dir, STAGING_DIR)?, STAGING_DIR)?,
            })
        }
    };
    Ok(Config {
        path: path.to_path_buf(),
        discogs_token,
        player_token,
        data_dir,
        downloads,
    })
}

/// The checks on one file's values, each error naming the file.
struct File<'a> {
    path: &'a Path,
}

impl File<'_> {
    /// Removes `key` from `table`, a string or nothing. A key in a table is named in full,
    /// `slskd.url`, and looked up by its last part.
    fn take(
        &self,
        table: &mut toml::Table,
        key: &'static str,
    ) -> Result<Option<String>, ConfigError> {
        let name = key.rsplit('.').next().unwrap_or(key);
        match table.remove(name) {
            None => Ok(None),
            Some(toml::Value::String(value)) if value.is_empty() => Err(ConfigError::Empty {
                path: self.path.to_path_buf(),
                key,
            }),
            Some(toml::Value::String(value)) => Ok(Some(value)),
            Some(_) => Err(ConfigError::NotAString {
                path: self.path.to_path_buf(),
                key,
            }),
        }
    }

    /// Removes the table `key` from `table`, or nothing.
    fn take_table(
        &self,
        table: &mut toml::Table,
        key: &'static str,
    ) -> Result<Option<toml::Table>, ConfigError> {
        match table.remove(key) {
            None => Ok(None),
            Some(toml::Value::Table(inner)) => Ok(Some(inner)),
            Some(_) => Err(ConfigError::NotATable {
                path: self.path.to_path_buf(),
                key,
            }),
        }
    }

    /// Refuses whatever `table` still holds after its known keys were taken; `prefix` is the
    /// table's name, empty for the top level.
    fn no_more(&self, table: &toml::Table, prefix: &str) -> Result<(), ConfigError> {
        match table.keys().next() {
            None => Ok(()),
            Some(key) => Err(ConfigError::UnknownKey {
                path: self.path.to_path_buf(),
                key: if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                },
            }),
        }
    }

    fn required(&self, value: Option<String>, key: &'static str) -> Result<String, ConfigError> {
        value.ok_or(ConfigError::MissingKey {
            path: self.path.to_path_buf(),
            key,
        })
    }

    fn absolute(&self, value: String, key: &'static str) -> Result<PathBuf, ConfigError> {
        let dir = PathBuf::from(value);
        if dir.is_absolute() {
            Ok(dir)
        } else {
            Err(ConfigError::NotAbsolute {
                path: self.path.to_path_buf(),
                key,
            })
        }
    }

    fn half(&self, present: &'static str, missing: &'static str) -> ConfigError {
        ConfigError::HalfConfigured {
            path: self.path.to_path_buf(),
            present,
            missing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Secret;

    #[test]
    fn the_token_prints_as_stars() {
        let token = Secret("abc123".to_string());
        assert_eq!(format!("{token:?}"), "***");
    }
}
