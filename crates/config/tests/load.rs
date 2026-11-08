//! Each refusal of `config::load`, on files written to a temp directory.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use config::ConfigError;

#[path = "support/setup.rs"]
mod setup;

const TOKEN: &str = "made-up-token-4f9a";
const PLAYER_TOKEN: &str = "made-up-player-token-77c1";
const SLSKD_KEY: &str = "made-up-slskd-key-2b8e";
const SECRETS: [&str; 3] = [TOKEN, PLAYER_TOKEN, SLSKD_KEY];

/// Writes `contents` as `config.toml` with `mode` in a fresh directory named for the test.
fn write(test: &str, contents: &str, mode: u32) -> PathBuf {
    let dir = setup::fresh_dir("config", test);
    let path = dir.join("config.toml");
    fs::write(&path, contents).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
    path
}

fn valid() -> String {
    format!(
        "discogs_token = \"{TOKEN}\"\nplayer_token = \"{PLAYER_TOKEN}\"\n\
         data_dir = \"/Users/someone/data\"\n"
    )
}

const SLSKD: &str = "[slskd]\nurl = \"http://127.0.0.1:5030\"\n\
                     downloads_dir = \"/Users/someone/soulseek/downloads\"\n";
const STAGING: &str = "[staging]\ndir = \"/Users/someone/mnt/music/staging\"\n";

/// A valid file with the downloads tables.
fn with_downloads() -> String {
    format!("{}{}api_key = \"{SLSKD_KEY}\"\n{STAGING}", valid(), SLSKD)
}

/// Loads and returns the error's message, checking it names the file and leaks no token.
fn refusal(path: &Path) -> (ConfigError, String) {
    let error = config::load(path).unwrap_err();
    let message = error.to_string();
    assert!(message.contains(&path.display().to_string()), "{message}");
    for secret in SECRETS {
        assert!(!message.contains(secret), "{message}");
        assert!(!format!("{error:?}").contains(secret), "{error:?}");
    }
    (error, message)
}

#[test]
fn reads_a_valid_file() {
    let path = write("valid", &valid(), 0o600);
    let config = config::load(&path).unwrap();
    assert_eq!(config.discogs_token.secret(), TOKEN);
    assert_eq!(config.player_token.secret(), PLAYER_TOKEN);
    assert_eq!(config.data_dir, PathBuf::from("/Users/someone/data"));
    assert_eq!(config.path, path);
    assert!(config.downloads.is_none());
    assert!(!format!("{config:?}").contains(TOKEN));
}

#[test]
fn reads_the_downloads_tables() {
    let path = write("downloads", &with_downloads(), 0o600);
    let config = config::load(&path).unwrap();
    let downloads = config.downloads.as_ref().unwrap();
    assert_eq!(downloads.slskd_url, "http://127.0.0.1:5030");
    assert_eq!(downloads.slskd_api_key.secret(), SLSKD_KEY);
    assert_eq!(
        downloads.slskd_downloads_dir,
        PathBuf::from("/Users/someone/soulseek/downloads")
    );
    assert_eq!(
        downloads.staging_dir,
        PathBuf::from("/Users/someone/mnt/music/staging")
    );
    let debug = format!("{config:?}");
    for secret in SECRETS {
        assert!(!debug.contains(secret), "{debug}");
    }
    assert!(debug.contains("***"), "{debug}");
}

#[test]
fn refuses_one_downloads_table_without_the_other() {
    let slskd = format!("{}{SLSKD}api_key = \"{SLSKD_KEY}\"\n", valid());
    let staging = format!("{}{STAGING}", valid());
    for (name, text, missing) in [
        ("only_slskd", slskd, "staging"),
        ("only_staging", staging, "slskd"),
    ] {
        let path = write(name, &text, 0o600);
        let (error, message) = refusal(&path);
        assert!(
            matches!(error, ConfigError::HalfConfigured { .. }),
            "{name}: {error:?}"
        );
        assert!(
            message.contains(&format!("[{missing}]")),
            "{name}: {message}"
        );
    }
}

#[test]
fn refuses_each_missing_key_in_the_tables() {
    for key in ["url", "api_key", "downloads_dir", "dir"] {
        let text: String = with_downloads()
            .lines()
            .filter(|line| !line.starts_with(&format!("{key} =")))
            .fold(String::new(), |text, line| text + line + "\n");
        let path = write(&format!("missing_table_{key}"), &text, 0o600);
        let (error, message) = refusal(&path);
        assert!(
            matches!(error, ConfigError::MissingKey { .. }),
            "{key}: {error:?}"
        );
        let table = if key == "dir" { "staging" } else { "slskd" };
        assert!(message.contains(&format!("`{table}.{key}`")), "{message}");
    }
}

#[test]
fn refuses_an_unknown_key_in_a_table() {
    let text = format!("{}minutes = \"5\"\n", with_downloads());
    let path = write("unknown_in_table", &text, 0o600);
    let (error, message) = refusal(&path);
    assert!(matches!(error, ConfigError::UnknownKey { .. }), "{error:?}");
    assert!(message.contains("`staging.minutes`"), "{message}");
}

#[test]
fn refuses_downloads_values_of_the_wrong_shape() {
    let base = valid();
    let key = format!("api_key = \"{SLSKD_KEY}\"\n");
    let cases = [
        (
            "slskd_not_a_table",
            format!("slskd = \"x\"\n{base}{STAGING}"),
            "slskd",
        ),
        (
            "key_not_a_string",
            format!("{base}{SLSKD}api_key = 5\n{STAGING}"),
            "slskd.api_key",
        ),
        (
            "url_not_http",
            format!(
                "{base}[slskd]\nurl = \"127.0.0.1:5030\"\n{key}\
                 downloads_dir = \"/d\"\n{STAGING}"
            ),
            "slskd.url",
        ),
        (
            "relative_downloads",
            format!(
                "{base}[slskd]\nurl = \"http://127.0.0.1:5030\"\n{key}\
                 downloads_dir = \"downloads\"\n{STAGING}"
            ),
            "slskd.downloads_dir",
        ),
        (
            "relative_staging",
            format!("{base}{SLSKD}{key}[staging]\ndir = \"staging\"\n"),
            "staging.dir",
        ),
    ];
    for (name, text, key) in cases {
        let path = write(name, &text, 0o600);
        let (error, message) = refusal(&path);
        assert!(message.contains(&format!("`{key}`")), "{name}: {message}");
        assert!(
            matches!(
                (name, &error),
                ("slskd_not_a_table", ConfigError::NotATable { .. })
                    | ("key_not_a_string", ConfigError::NotAString { .. })
                    | ("url_not_http", ConfigError::NotAUrl { .. })
                    | (
                        "relative_downloads" | "relative_staging",
                        ConfigError::NotAbsolute { .. }
                    )
            ),
            "{name}: {error:?}"
        );
    }
}

#[test]
fn refuses_a_file_group_or_others_can_read() {
    for mode in [0o640, 0o604, 0o644, 0o620] {
        let path = write(&format!("mode_{mode:o}"), &valid(), mode);
        let (error, message) = refusal(&path);
        assert!(matches!(error, ConfigError::Permissions { .. }), "{mode:o}");
        assert!(message.contains("chmod 600"), "{message}");
    }
}

#[test]
fn refuses_a_missing_file() {
    let path = write("missing", "", 0o600).with_file_name("absent.toml");
    let (error, _) = refusal(&path);
    assert!(matches!(error, ConfigError::Read { .. }));
}

#[test]
fn refuses_each_missing_key() {
    for key in ["discogs_token", "player_token", "data_dir"] {
        let text: String = valid()
            .lines()
            .filter(|line| !line.starts_with(key))
            .fold(String::new(), |text, line| text + line + "\n");
        let path = write(&format!("missing_{key}"), &text, 0o600);
        let (error, message) = refusal(&path);
        assert!(matches!(error, ConfigError::MissingKey { .. }), "{key}");
        assert!(message.contains(key), "{message}");
    }
}

#[test]
fn refuses_an_unknown_key_without_printing_its_value() {
    // A misspelled token key holds the token, so the message names the key only.
    let text = valid().replace("discogs_token", "discogs_tokn");
    let path = write("unknown", &text, 0o600);
    let (error, message) = refusal(&path);
    assert!(matches!(error, ConfigError::UnknownKey { .. }));
    assert!(message.contains("discogs_tokn"), "{message}");
}

#[test]
fn refuses_bad_toml_without_quoting_the_line() {
    let text = format!("data_dir = \"/Users/someone/data\"\ndiscogs_token = \"{TOKEN}\n");
    let path = write("syntax", &text, 0o600);
    let (error, message) = refusal(&path);
    assert!(
        matches!(error, ConfigError::Syntax { line: 2, .. }),
        "{error:?}"
    );
    assert!(message.contains(":2:"), "{message}");
}

#[test]
fn refuses_values_of_the_wrong_shape() {
    let cases = [
        (
            "not_a_string",
            "discogs_token = 12345\nplayer_token = \"p\"\ndata_dir = \"/d\"\n",
            "discogs_token",
        ),
        (
            "empty",
            "discogs_token = \"\"\nplayer_token = \"p\"\ndata_dir = \"/d\"\n",
            "discogs_token",
        ),
        (
            "relative",
            "discogs_token = \"t\"\nplayer_token = \"p\"\ndata_dir = \"data\"\n",
            "data_dir",
        ),
    ];
    for (name, text, key) in cases {
        let path = write(name, text, 0o600);
        let (error, message) = refusal(&path);
        assert!(message.contains(key), "{name}: {message}");
        assert!(
            matches!(
                (name, &error),
                ("not_a_string", ConfigError::NotAString { .. })
                    | ("empty", ConfigError::Empty { .. })
                    | ("relative", ConfigError::NotAbsolute { .. })
            ),
            "{name}: {error:?}"
        );
    }
}

#[test]
fn reads_the_api_port_or_its_default() {
    let path = write("port_default", &valid(), 0o600);
    // 7878 is the documented default (ADR 0008), which the player's Settings suggest.
    assert_eq!(config::load(&path).unwrap().api_port, 7878);
    let path = write("port_set", &format!("api_port = 8123\n{}", valid()), 0o600);
    assert_eq!(config::load(&path).unwrap().api_port, 8123);
}

#[test]
fn refuses_a_port_out_of_range_or_not_a_number() {
    for (name, value) in [
        ("zero", "0"),
        ("high", "65536"),
        ("negative", "-1"),
        ("text", "\"80\""),
    ] {
        let path = write(
            &format!("port_{name}"),
            &format!("api_port = {value}\n{}", valid()),
            0o600,
        );
        let (error, message) = refusal(&path);
        assert!(
            matches!(error, ConfigError::NotAPort { .. }),
            "{name}: {error:?}"
        );
        assert!(message.contains("`api_port`"), "{message}");
    }
}
