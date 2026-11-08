//! Reads the service's configuration, the one place that does, and fails on a missing key.
//!
//! The file is `~/Library/Application Support/music-recommender/config.toml`, owner-only
//! (mode 0600), with these keys:
//!
//! | Key | Required | What it is |
//! |---|---|---|
//! | `discogs_token` | yes | The owner's Discogs personal access token. |
//! | `data_dir` | yes | An absolute path to an existing directory: the community database and the sync's lock file live there. |
//!
//! Any other key is refused, so a misspelled key fails instead of being ignored.

use std::fmt;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// The keys this file accepts.
const DISCOGS_TOKEN: &str = "discogs_token";
const DATA_DIR: &str = "data_dir";

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
    /// `data_dir`: where the community database and the sync's lock file live.
    pub data_dir: PathBuf,
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
    /// A key's value is the empty string.
    Empty { path: PathBuf, key: &'static str },
    /// `data_dir` is a relative path, which would depend on where the service started.
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
    let mut take = |key: &'static str| -> Result<Option<String>, ConfigError> {
        match table.remove(key) {
            None => Ok(None),
            Some(toml::Value::String(value)) if value.is_empty() => Err(ConfigError::Empty {
                path: path.to_path_buf(),
                key,
            }),
            Some(toml::Value::String(value)) => Ok(Some(value)),
            Some(_) => Err(ConfigError::NotAString {
                path: path.to_path_buf(),
                key,
            }),
        }
    };
    let required = |value: Option<String>, key| {
        value.ok_or(ConfigError::MissingKey {
            path: path.to_path_buf(),
            key,
        })
    };
    let discogs_token = take(DISCOGS_TOKEN)?;
    let data_dir = take(DATA_DIR)?;
    if let Some(key) = table.keys().next() {
        return Err(ConfigError::UnknownKey {
            path: path.to_path_buf(),
            key: key.clone(),
        });
    }
    let discogs_token = Secret(required(discogs_token, DISCOGS_TOKEN)?);
    let data_dir = PathBuf::from(required(data_dir, DATA_DIR)?);
    if !data_dir.is_absolute() {
        return Err(ConfigError::NotAbsolute {
            path: path.to_path_buf(),
            key: DATA_DIR,
        });
    }
    Ok(Config {
        path: path.to_path_buf(),
        discogs_token,
        data_dir,
    })
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
