//! Candidates on the catalog imported from the sources crate's fixture set: master 100 has
//! the pressings 1001, its `main_release`, and 1002, and release 1003 has no master.

#[path = "support/catalog.rs"]
mod catalog_support;
#[path = "support/clock.rs"]
mod clock;
#[path = "../../config/tests/support/setup.rs"]
mod support;

use catalog::discogs::Catalog;
use catalog_support::tagged;
use features::candidate::Candidate;
use features::connection::Connections;
use features::library::Library;
use features::profile::Profile;
use features::seed::Seeds;

/// The catalog imported from the fixture set, in a fresh directory named for the test.
fn catalog(test: &str) -> Catalog {
    catalog_support::imported("candidate", test, catalog_support::fixture_set)
}

/// The candidate release `id` is part of.
fn read(catalog: &Catalog, id: u64) -> Candidate {
    Candidate::read(catalog, id).unwrap().unwrap()
}

/// A library of one album tagged with each of `ids`.
fn library_of(ids: &[u64]) -> Profile {
    Profile {
        albums: ids.iter().map(|&id| tagged(id, 0)).collect(),
    }
}

/// The candidates the connections of the seeds tagged with `ids` find.
fn connected(catalog: &Catalog, ids: &[u64]) -> Connections {
    let seeds = Seeds::read(&library_of(ids), catalog).unwrap();
    Connections::read(catalog, &seeds).unwrap()
}

#[test]
fn pressings_of_one_master_are_one_candidate() {
    let catalog = catalog("pressings");
    assert_eq!(read(&catalog, 1001), Candidate::Master(100));
    assert_eq!(read(&catalog, 1002), Candidate::Master(100));
    // Ann's 1003 connects to both pressings, by the band she is a member of and by her
    // credit on 1001.
    let found = connected(&catalog, &[1003]).candidates;
    assert!(found.contains_key(&Candidate::Master(100)), "{found:?}");
    assert!(!found.contains_key(&Candidate::Release(1001)), "{found:?}");
    assert!(!found.contains_key(&Candidate::Release(1002)), "{found:?}");
}

#[test]
fn a_release_without_a_master_is_a_candidate_of_its_own() {
    let catalog = catalog("masterless");
    assert_eq!(read(&catalog, 1003), Candidate::Release(1003));
    // The band's 1001 connects to Ann's 1003, a member's.
    let found = connected(&catalog, &[1001]).candidates;
    assert!(found.contains_key(&Candidate::Release(1003)), "{found:?}");
    // A release the catalog doesn't have is no candidate.
    assert_eq!(Candidate::read(&catalog, 9999).unwrap(), None);
}

#[test]
fn a_master_s_representative_is_its_most_owned_version_or_else_its_main_release() {
    let catalog = catalog("representative");
    let representative = |candidate: Candidate, most_owned| {
        let entry = candidate.entry(&catalog).unwrap()?;
        candidate.representative(&entry, most_owned)
    };
    assert_eq!(representative(Candidate::Master(100), None), Some(1001));
    assert_eq!(
        representative(Candidate::Master(100), Some(1002)),
        Some(1002)
    );
    assert_eq!(
        representative(Candidate::Release(1003), Some(9)),
        Some(1003)
    );
    assert_eq!(representative(Candidate::Master(999), None), None);
}

#[test]
fn a_candidate_whose_master_or_release_is_tagged_in_the_library_is_the_library_s() {
    let catalog = catalog("library");
    // The library has a pressing of master 100 that isn't its main release.
    let library = Library::new(&library_of(&[1002]), &catalog).unwrap();
    assert!(library.has_id(read(&catalog, 1001)));
    assert!(!library.has_id(read(&catalog, 1003)));
    let library = Library::new(&library_of(&[1003]), &catalog).unwrap();
    assert!(library.has_id(read(&catalog, 1003)));
    assert!(!library.has_id(read(&catalog, 1001)));
    // A tagged release the catalog doesn't have adds no master.
    let library = Library::new(&library_of(&[9999]), &catalog).unwrap();
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
    let catalog = catalog_support::open(&config.data_dir);
    let entry = Candidate::Master(100).entry(&catalog).unwrap().unwrap();
    assert_eq!(entry.title, "First Album");
    assert_eq!(entry.artists.len(), 1);
    // A master with no row and no releases has none.
    assert_eq!(Candidate::Master(999).entry(&catalog).unwrap(), None);
}
