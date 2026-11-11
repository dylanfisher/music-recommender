//! Candidates on the catalog imported from the sources crate's fixture set: master 100 has
//! the pressings 1001, its `main_release`, and 1002, and release 1003 has no master.

#[path = "support/clock.rs"]
mod clock;
#[path = "../../config/tests/support/setup.rs"]
mod support;

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use catalog::discogs::{Catalog, Reading};
use clock::FakeClock;
use features::candidate::{self, Candidate};
use features::import;
use features::library::Library;
use features::profile::{Album, Profile};

/// The catalog imported from a copy of the fixture set, in a fresh directory named for the
/// test.
fn catalog(test: &str) -> Catalog {
    let dir = support::fresh_dir("candidate", test);
    let (path, data_dir) = support::write_config(&dir);
    let dumps = dir.join("discogs");
    fs::create_dir_all(&dumps).unwrap();
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../sources/tests/fixtures");
    for entry in fs::read_dir(fixtures.join("dumps")).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), dumps.join(entry.file_name())).unwrap();
    }
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(format!("[dumps]\ndir = \"{}\"\n", dumps.display()).as_bytes())
        .unwrap();
    let config = config::load(&path).unwrap();
    import::run(&config, &FakeClock::new(), 100, &mut |_| {}).unwrap();
    let Reading::Open(catalog) = Catalog::open_read_only(&data_dir).unwrap() else {
        panic!("no catalog");
    };
    catalog
}

/// The candidate release `id` is part of.
fn read(catalog: &Catalog, id: u64) -> Candidate {
    Candidate::read(catalog, id).unwrap().unwrap()
}

/// A library of one album tagged with each of `ids`.
fn tagged(ids: &[u64]) -> Profile {
    Profile {
        albums: ids
            .iter()
            .map(|&id| Album {
                artist: "Someone Else".to_string(),
                title: format!("Album {id}"),
                track_count: 1,
                durations: vec![60],
                catalog_numbers: Vec::new(),
                barcodes: Vec::new(),
                discogs_release_id: Some(id),
                plays: 0,
                loved: 0,
                rating: None,
            })
            .collect(),
    }
}

#[test]
fn pressings_of_one_master_are_one_candidate_with_the_best_of_their_connections() {
    let catalog = catalog("pressings");
    assert_eq!(read(&catalog, 1001), Candidate::Master(100));
    assert_eq!(read(&catalog, 1002), Candidate::Master(100));
    let collapsed = candidate::collapse([
        (read(&catalog, 1001), "producer", 1.0),
        (read(&catalog, 1002), "producer", 3.0),
        (read(&catalog, 1001), "label", 2.0),
        (read(&catalog, 1002), "label", 0.5),
        (read(&catalog, 1002), "style", 0.25),
    ]);
    assert_eq!(
        collapsed,
        [(
            Candidate::Master(100),
            [("producer", 3.0), ("label", 2.0), ("style", 0.25)].into()
        )]
        .into()
    );
}

#[test]
fn a_release_without_a_master_is_a_candidate_of_its_own() {
    let catalog = catalog("masterless");
    assert_eq!(read(&catalog, 1003), Candidate::Release(1003));
    let collapsed = candidate::collapse([
        (read(&catalog, 1003), "producer", 1.0),
        (read(&catalog, 1001), "producer", 2.0),
    ]);
    assert_eq!(
        collapsed[&Candidate::Release(1003)],
        [("producer", 1.0)].into()
    );
    assert_eq!(
        collapsed[&Candidate::Master(100)],
        [("producer", 2.0)].into()
    );
    // A release the catalog doesn't have is no candidate.
    assert_eq!(Candidate::read(&catalog, 9999).unwrap(), None);
}

#[test]
fn a_master_s_representative_is_its_main_release() {
    let catalog = catalog("representative");
    assert_eq!(
        Candidate::Master(100).representative(&catalog).unwrap(),
        Some(1001)
    );
    assert_eq!(
        Candidate::Release(1003).representative(&catalog).unwrap(),
        Some(1003)
    );
    assert_eq!(
        Candidate::Master(999).representative(&catalog).unwrap(),
        None
    );
}

#[test]
fn a_candidate_whose_master_or_release_is_tagged_in_the_library_is_the_library_s() {
    let catalog = catalog("library");
    // The library has a pressing of master 100 that isn't its main release.
    let library = Library::new(&tagged(&[1002]), &catalog).unwrap();
    assert!(library.has_id(read(&catalog, 1001)));
    assert!(!library.has_id(read(&catalog, 1003)));
    let library = Library::new(&tagged(&[1003]), &catalog).unwrap();
    assert!(library.has_id(read(&catalog, 1003)));
    assert!(!library.has_id(read(&catalog, 1001)));
    // A tagged release the catalog doesn't have adds no master.
    let library = Library::new(&tagged(&[9999]), &catalog).unwrap();
    assert!(!library.has_id(read(&catalog, 1001)));
}
