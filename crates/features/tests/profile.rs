//! Reading profile files, keeping the last one and queueing their release ids.

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;

use catalog::community::Community;
use config::Config;

#[path = "../../config/tests/support/setup.rs"]
mod support;
use features::profile::{self, Album, Profile, ProfileError, Queued};

/// The size cap docs/api.md gives.
const MAX_BYTES: u64 = 32 * 1024 * 1024;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// A fresh directory named for the test, with a config whose data directory is in it.
fn setup(test: &str) -> (PathBuf, Config) {
    let dir = support::fresh_dir("profile", test);
    let (path, _) = support::write_config(&dir);
    (dir, config::load(&path).unwrap())
}

#[test]
fn queues_the_release_ids_of_a_valid_profile_in_order() {
    let (_, config) = setup("valid");
    let profile = profile::load(&fixture("profile.json")).unwrap();
    assert_eq!(profile.albums.len(), 3);
    assert_eq!(profile.albums[0].durations, [245, 750]);
    assert_eq!(profile.albums[0].rating, Some(4));
    assert_eq!(profile.albums[1].discogs_release_id, None);
    assert_eq!(
        profile::queue(&profile, &config).unwrap(),
        Queued {
            albums: 3,
            with_id: 2,
            added: 2
        }
    );
    let db = Community::open(&config.data_dir).unwrap();
    assert_eq!(db.next_due(0).unwrap(), Some(4242));
    // Loading it again queues nothing new.
    assert_eq!(profile::queue(&profile, &config).unwrap().added, 0);
    assert_eq!(db.progress(0).unwrap().total().queued, 2);
}

#[test]
fn a_profile_with_no_ids_queues_nothing() {
    let (_, config) = setup("no_ids");
    let profile = profile::load(&fixture("profile-no-ids.json")).unwrap();
    assert_eq!(
        profile::queue(&profile, &config).unwrap(),
        Queued {
            albums: 1,
            with_id: 0,
            added: 0
        }
    );
    let db = Community::open(&config.data_dir).unwrap();
    assert_eq!(db.next_due(0).unwrap(), None);
}

/// Loads `text` written to a file in `dir` and returns the refusal's message, checking
/// it names the file.
fn refusal(dir: &Path, text: &str) -> (ProfileError, String) {
    let path = dir.join("profile.json");
    fs::write(&path, text).unwrap();
    let error = profile::load(&path).unwrap_err();
    let message = error.to_string();
    assert!(message.contains(&path.display().to_string()), "{message}");
    (error, message)
}

#[test]
fn refuses_a_malformed_profile() {
    let (dir, _) = setup("malformed");
    let valid = fs::read_to_string(fixture("profile.json")).unwrap();

    let (error, message) = refusal(&dir, &valid[..valid.len() / 2]);
    assert!(matches!(error, ProfileError::Json(..)), "{message}");
    assert!(message.contains("line"), "{message}");

    let (error, message) = refusal(&dir, &valid.replace("\"plays\": 31,", ""));
    assert!(matches!(error, ProfileError::Json(..)), "{message}");
    assert!(message.contains("plays"), "{message}");

    let (error, message) = refusal(&dir, &valid.replace("\"loved\": 1,", "\"lovd\": 1,"));
    assert!(matches!(error, ProfileError::Json(..)), "{message}");
    assert!(message.contains("lovd"), "{message}");

    let (error, message) = refusal(&dir, &valid.replace("4243", "\"4243\""));
    assert!(matches!(error, ProfileError::Json(..)), "{message}");

    let (error, message) = refusal(&dir, &valid.replace("4243", "0"));
    assert!(matches!(error, ProfileError::Invalid(_, 2, _)), "{message}");

    let (error, message) = refusal(&dir, &valid.replace("4243", "9223372036854775808"));
    assert!(matches!(error, ProfileError::Invalid(_, 2, _)), "{message}");
    assert!(message.contains("albums[2]"), "{message}");

    let (error, message) = refusal(&dir, &valid.replace("\"rating\": 4", "\"rating\": 6"));
    assert!(matches!(error, ProfileError::Invalid(_, 0, _)), "{message}");
}

#[test]
fn refuses_a_profile_over_the_size_cap() {
    let (dir, _) = setup("oversized");
    let path = dir.join("profile.json");
    let valid = fs::read(fixture("profile.json")).unwrap();
    fs::write(&path, &valid).unwrap();
    // Padding past the cap; a sparse file, so nothing large is written.
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(MAX_BYTES + 1)
        .unwrap();
    let error = profile::load(&path).unwrap_err();
    assert!(matches!(error, ProfileError::TooLarge(_)), "{error}");
    assert!(error.to_string().contains("32 MB"), "{error}");
}

#[test]
fn keeps_a_profile_and_reads_it_back() {
    let (_, config) = setup("kept");
    assert!(profile::kept(&config.data_dir).unwrap().is_none());
    let loaded = profile::load(&fixture("profile.json")).unwrap();
    profile::keep(&loaded, &config.data_dir).unwrap();
    assert_eq!(profile::kept(&config.data_dir).unwrap(), Some(loaded));
}

#[test]
fn a_second_profile_replaces_the_first_whole() {
    let (_, config) = setup("replaced");
    profile::keep(
        &profile::load(&fixture("profile.json")).unwrap(),
        &config.data_dir,
    )
    .unwrap();
    let second = profile::load(&fixture("profile-no-ids.json")).unwrap();
    profile::keep(&second, &config.data_dir).unwrap();
    assert_eq!(profile::kept(&config.data_dir).unwrap(), Some(second));
    // Only the kept file is left: no partly written one beside it.
    assert_eq!(support::files(&config.data_dir), ["profile.json"]);
}

/// A profile of `albums` albums by `artist`, large enough that two writes overlap.
fn big(artist: &str, albums: u32) -> Profile {
    Profile {
        albums: (0..albums)
            .map(|index| Album {
                artist: artist.to_string(),
                title: format!("Album {index}"),
                track_count: 1,
                durations: vec![60],
                catalog_numbers: Vec::new(),
                barcodes: Vec::new(),
                discogs_release_id: Some(u64::from(index) + 1),
                plays: 1,
                loved: 0,
                rating: None,
            })
            .collect(),
    }
}

#[test]
fn two_keeps_at_once_leave_one_of_them_whole() {
    let (_, config) = setup("concurrent");
    let (first, second) = (big("First", 3000), big("Second", 3000));
    thread::scope(|scope| {
        for profile in [&first, &second] {
            let data_dir = &config.data_dir;
            scope.spawn(move || {
                for _ in 0..40 {
                    profile::keep(profile, data_dir).unwrap();
                }
            });
        }
    });
    let kept = profile::kept(&config.data_dir).unwrap().unwrap();
    assert!(kept == first || kept == second);
    assert_eq!(support::files(&config.data_dir), ["profile.json"]);
}

#[test]
fn a_profile_at_the_cap_is_kept_and_read_back() {
    let (_, config) = setup("at-the-cap");
    // An album without an id or a rating, which `keep` writes with both as `null`, so
    // what is kept is longer than what was accepted.
    let album = |title: &str| {
        format!(
            r#"{{"albums":[{{"artist":"A","title":"{title}","track_count":1,"durations":[60],"catalog_numbers":[],"barcodes":[],"plays":1,"loved":0}}]}}"#
        )
    };
    let padding = usize::try_from(MAX_BYTES).unwrap() - album("").len();
    let json = album(&"x".repeat(padding));
    assert_eq!(json.len() as u64, MAX_BYTES);
    let accepted = profile::parse(json.as_bytes()).unwrap();
    profile::keep(&accepted, &config.data_dir).unwrap();
    assert!(
        config
            .data_dir
            .join("profile.json")
            .metadata()
            .unwrap()
            .len()
            > MAX_BYTES
    );
    assert_eq!(profile::kept(&config.data_dir).unwrap(), Some(accepted));
}

#[test]
fn a_kept_profile_that_doesnt_parse_is_discarded_with_an_error() {
    let (_, config) = setup("discarded");
    let path = config.data_dir.join("profile.json");
    let valid = fs::read_to_string(fixture("profile.json")).unwrap();
    fs::write(&path, valid.replace("\"plays\": 31,", "")).unwrap();
    let error = profile::kept(&config.data_dir).unwrap_err();
    assert!(matches!(error, ProfileError::Discarded(..)), "{error}");
    let message = error.to_string();
    assert!(message.contains(&path.display().to_string()), "{message}");
    assert!(message.contains("plays"), "{message}");
    assert!(message.contains("deleted"), "{message}");
    assert!(!path.exists());
    // Discarded once, it reads as no profile.
    assert!(profile::kept(&config.data_dir).unwrap().is_none());
}
