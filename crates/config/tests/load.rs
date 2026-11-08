//! Each refusal of `config::load`, on files written to a temp directory.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use config::ConfigError;

#[path = "support/setup.rs"]
mod setup;

const TOKEN: &str = "made-up-token-4f9a";

/// Writes `contents` as `config.toml` with `mode` in a fresh directory named for the test.
fn write(test: &str, contents: &str, mode: u32) -> PathBuf {
    let dir = setup::fresh_dir("config", test);
    let path = dir.join("config.toml");
    fs::write(&path, contents).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
    path
}

fn valid() -> String {
    format!("discogs_token = \"{TOKEN}\"\ndata_dir = \"/Users/someone/data\"\n")
}

/// Loads and returns the error's message, checking it names the file and leaks no token.
fn refusal(path: &Path) -> (ConfigError, String) {
    let error = config::load(path).unwrap_err();
    let message = error.to_string();
    assert!(message.contains(&path.display().to_string()), "{message}");
    assert!(!message.contains(TOKEN), "{message}");
    assert!(!format!("{error:?}").contains(TOKEN), "{error:?}");
    (error, message)
}

#[test]
fn reads_a_valid_file() {
    let path = write("valid", &valid(), 0o600);
    let config = config::load(&path).unwrap();
    assert_eq!(config.discogs_token.secret(), TOKEN);
    assert_eq!(config.data_dir, PathBuf::from("/Users/someone/data"));
    assert_eq!(config.path, path);
    assert!(!format!("{config:?}").contains(TOKEN));
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
    for key in ["discogs_token", "data_dir"] {
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
    let text = format!("discogs_tokn = \"{TOKEN}\"\ndata_dir = \"/Users/someone/data\"\n");
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
            "discogs_token = 12345\ndata_dir = \"/d\"\n",
            "discogs_token",
        ),
        (
            "empty",
            "discogs_token = \"\"\ndata_dir = \"/d\"\n",
            "discogs_token",
        ),
        (
            "relative",
            "discogs_token = \"t\"\ndata_dir = \"data\"\n",
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
