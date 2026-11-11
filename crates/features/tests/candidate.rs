//! Candidates on the catalog imported from the sources crate's fixture set: master 100 has
//! the pressings 1001, its `main_release`, and 1002, and release 1003 has no master.

#[path = "support/catalog.rs"]
mod catalog_support;
#[path = "support/clock.rs"]
mod clock;
#[path = "../../config/tests/support/setup.rs"]
mod support;

use catalog::discogs::{Catalog, Reading};
use features::candidate::{self, Candidate};
use features::library::Library;
use features::profile::{Album, Profile};

/// The catalog imported from the fixture set, in a fresh directory named for the test.
fn catalog(test: &str) -> Catalog {
    catalog_support::imported("candidate", test, catalog_support::fixture_set)
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
fn pressings_of_one_master_are_one_candidate_with_each_shared_connection_once() {
    let catalog = catalog("pressings");
    assert_eq!(read(&catalog, 1001), Candidate::Master(100));
    assert_eq!(read(&catalog, 1002), Candidate::Master(100));
    let collapsed = candidate::collapse([
        (read(&catalog, 1001), "producer"),
        (read(&catalog, 1002), "producer"),
        (read(&catalog, 1001), "label"),
        (read(&catalog, 1002), "label"),
        (read(&catalog, 1002), "style"),
    ]);
    assert_eq!(
        collapsed,
        [(
            Candidate::Master(100),
            ["producer", "label", "style"].into()
        )]
        .into()
    );
}

#[test]
fn a_release_without_a_master_is_a_candidate_of_its_own() {
    let catalog = catalog("masterless");
    assert_eq!(read(&catalog, 1003), Candidate::Release(1003));
    let collapsed = candidate::collapse([
        (read(&catalog, 1003), "producer"),
        (read(&catalog, 1001), "producer"),
    ]);
    assert_eq!(collapsed[&Candidate::Release(1003)], ["producer"].into());
    assert_eq!(collapsed[&Candidate::Master(100)], ["producer"].into());
    // A release the catalog doesn't have is no candidate.
    assert_eq!(Candidate::read(&catalog, 9999).unwrap(), None);
}

#[test]
fn a_master_s_representative_is_its_main_release() {
    let catalog = catalog("representative");
    let representative = |candidate: Candidate| {
        let entry = candidate.entry(&catalog).unwrap()?;
        candidate.representative(&entry)
    };
    assert_eq!(representative(Candidate::Master(100)), Some(1001));
    assert_eq!(representative(Candidate::Release(1003)), Some(1003));
    assert_eq!(representative(Candidate::Master(999)), None);
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

#[test]
fn a_master_the_catalog_has_no_row_for_is_shown_by_its_first_release() {
    let config = catalog_support::imported_config(
        "candidate",
        "no-master-row",
        catalog_support::fixture_set,
    );
    // The masters dump can lack a master its releases name.
    rusqlite::Connection::open(config.data_dir.join("catalog.sqlite"))
        .unwrap()
        .execute("DELETE FROM master WHERE id = 100", [])
        .unwrap();
    let Reading::Open(catalog) = Catalog::open_read_only(&config.data_dir).unwrap() else {
        panic!("no catalog");
    };
    let entry = Candidate::Master(100).entry(&catalog).unwrap().unwrap();
    assert_eq!(entry.title, "First Album");
    assert_eq!(entry.artists.len(), 1);
    // A master with no row and no releases has none.
    assert_eq!(Candidate::Master(999).entry(&catalog).unwrap(), None);
}
