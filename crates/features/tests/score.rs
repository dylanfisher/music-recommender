//! The catalog score on the catalog imported from the sources crate's fixture set. Seed
//! 2001 is Cora Seed's, on Seed Records in 2000, produced by Dee Producer and Eli Cutter,
//! styled Made-Up Rare and Made-Up Common; seed 1003 is Ann Example's, an alias of DJ Ann
//! and a member of The Made-Up Band; seed 1001 is the band's. The other releases are the
//! candidates each test names. The budget is measured on ADR 0022's generated set.

#[path = "support/catalog.rs"]
mod catalog_support;
#[path = "support/clock.rs"]
mod clock;
#[path = "support/generated.rs"]
mod generated;
#[path = "../../config/tests/support/setup.rs"]
mod support;

use std::collections::HashSet;

use catalog::discogs::Catalog;
use features::candidate::Candidate;
use features::connection::Connections;
use features::library::Library;
use features::profile::{Album, Profile};
use features::score::{self, Connection, Named, Reason, Scope, Scored};
use features::seed::{Relation, Seeds};

/// The catalog imported from the fixture set, in a fresh directory named for the test.
fn catalog(test: &str) -> Catalog {
    catalog_support::imported("score", test, catalog_support::fixture_set)
}

/// A library album by `artist`, titled `title`, tagged with `release_id`, played `plays`
/// times.
fn album(artist: &str, title: &str, release_id: Option<u64>, plays: u64) -> Album {
    Album {
        artist: artist.to_string(),
        title: title.to_string(),
        track_count: 1,
        durations: vec![180],
        catalog_numbers: Vec::new(),
        barcodes: Vec::new(),
        discogs_release_id: release_id,
        plays,
        loved: 0,
        rating: None,
    }
}

/// A seed album tagged with `release_id`, without plays.
fn seed(release_id: u64) -> Album {
    album(
        "Someone Else",
        &format!("Album {release_id}"),
        Some(release_id),
        0,
    )
}

/// The candidates of a library of `albums`, scored from every seed.
fn scored(catalog: &Catalog, albums: Vec<Album>) -> Vec<Scored> {
    let profile = Profile { albums };
    let seeds = Seeds::read(&profile, catalog).unwrap();
    let connections = Connections::read(catalog, &seeds).unwrap();
    let library = Library::new(&profile, catalog).unwrap();
    score::score(&seeds, &connections, &library, &Scope::Library)
}

/// The scored candidate, if it was scored.
fn find(scored: &[Scored], candidate: Candidate) -> Option<&Scored> {
    scored.iter().find(|s| s.candidate == candidate)
}

/// The scored candidate's reasons.
fn reasons(scored: &[Scored], candidate: Candidate) -> &[Reason] {
    &find(scored, candidate)
        .unwrap_or_else(|| panic!("{candidate:?} wasn't scored: {scored:#?}"))
        .reasons
}

fn named(id: u64, name: &str) -> Named {
    Named {
        id,
        name: Some(name.to_string()),
    }
}

/// Whether two weights are the same, to rounding.
fn same(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn an_album_by_a_seed_s_artist_connects_by_the_artist() {
    let scored = scored(&catalog("artist"), vec![seed(2001)]);
    let reasons = reasons(&scored, Candidate::Release(2002));
    assert_eq!(
        reasons[0].connection,
        Connection::Artist {
            artist: named(20, "Cora Seed"),
            albums: 1
        }
    );
    // The most-weighted seed artist's weight is 2.
    assert!(same(reasons[0].weight, 2.0), "{reasons:?}");
}

#[test]
fn a_person_credited_on_a_seed_connects_by_the_credit() {
    let scored = scored(&catalog("credit"), vec![seed(2001)]);
    let dee = &reasons(&scored, Candidate::Release(2003))[0];
    assert_eq!(
        dee.connection,
        Connection::Credit {
            artist: named(21, "Dee Producer"),
            role: Some("Producer".to_string()),
            seed_roles: vec!["Producer".to_string()],
            albums: 1
        }
    );
    // Producer on both, one seed, on 2 releases: 1 * 1 * sqrt(1) / log2(2 + 2) * 3.
    assert!(same(dee.weight, 1.5), "{dee:?}");
}

#[test]
fn a_credit_in_a_weak_role_weighs_a_fifth() {
    let scored = scored(&catalog("weak-role"), vec![seed(2001)]);
    let dee = &reasons(&scored, Candidate::Release(2003))[0];
    // Eli Cutter produced the seed and cut 2004's lacquer; on 2 releases, as Dee is.
    let eli = reasons(&scored, Candidate::Release(2004))
        .iter()
        .find(|reason| matches!(reason.connection, Connection::Credit { .. }))
        .unwrap();
    assert!(
        matches!(&eli.connection, Connection::Credit { artist, role, .. }
            if artist.id == 22 && role.as_deref() == Some("Lacquer Cut By")),
        "{eli:?}"
    );
    assert!(
        same(eli.weight, dee.weight / 5.0),
        "{eli:?} against {dee:?}"
    );
}

#[test]
fn a_label_s_family_connects_within_3_years_and_not_at_4() {
    let scored = scored(&catalog("label"), vec![seed(2001)]);
    // 2005 is on Seed Sub, Seed Records' sublabel, in 2003; the seed in 2000.
    let label = &reasons(&scored, Candidate::Release(2005))[0];
    assert_eq!(
        label.connection,
        Connection::Label {
            label: named(31, "Seed Sub"),
            via: Some(named(30, "Seed Records")),
            albums: 1
        }
    );
    // One seed, Seed Sub on 2 releases: sqrt(1) / log2(2 + 2) * 2.
    assert!(same(label.weight, 1.0), "{label:?}");
    // 2006 is on Seed Sub in 2004.
    assert!(find(&scored, Candidate::Release(2006)).is_none());
}

#[test]
fn an_alias_a_group_or_a_member_of_a_seed_s_artist_connects() {
    let catalog = catalog("related");
    let related = |scored: &[Scored], candidate| -> Vec<Connection> {
        reasons(scored, candidate)
            .iter()
            .filter(|reason| matches!(reason.connection, Connection::Related { .. }))
            .map(|reason| {
                assert!(same(reason.weight, 1.5), "{reason:?}");
                reason.connection.clone()
            })
            .collect()
    };
    let ann = named(2, "Ann Example");
    let band = named(1, "The Made-Up Band");
    let scored_ann = scored(&catalog, vec![seed(1003)]);
    assert_eq!(
        related(&scored_ann, Candidate::Release(2008)),
        [Connection::Related {
            artist: named(4, "DJ Ann"),
            relation: Relation::Alias,
            of: ann.clone()
        }]
    );
    assert_eq!(
        related(&scored_ann, Candidate::Master(100)),
        [Connection::Related {
            artist: band.clone(),
            relation: Relation::Group,
            of: ann.clone()
        }]
    );
    let scored_band = scored(&catalog, vec![seed(1001)]);
    assert_eq!(
        related(&scored_band, Candidate::Release(1003)),
        [Connection::Related {
            artist: ann,
            relation: Relation::Member,
            of: band
        }]
    );
}

#[test]
fn a_rare_style_the_seeds_have_weighs_more_than_a_common_one() {
    let scored = scored(&catalog("styles"), vec![seed(2001)]);
    let styles = |candidate| -> Reason {
        reasons(&scored, candidate)
            .iter()
            .find(|reason| matches!(reason.connection, Connection::Styles { .. }))
            .unwrap()
            .clone()
    };
    // The fixture's releases have 15 styles in all: Made-Up Rare and Made-Up Other 2 each,
    // Made-Up Common 8. Both candidates are Cora Seed's, with Made-Up Other, which the seed
    // hasn't; 2002 has Made-Up Rare and 2011 Made-Up Common, which it has.
    let rare = styles(Candidate::Release(2002));
    let common = styles(Candidate::Release(2011));
    let idf = |releases: f64| (15.0 / releases).ln();
    assert!(
        same(rare.weight, 0.5 * idf(2.0) / (idf(2.0) + idf(2.0))),
        "{rare:?}"
    );
    assert!(
        same(common.weight, 0.5 * idf(8.0) / (idf(8.0) + idf(2.0))),
        "{common:?}"
    );
    assert!(rare.weight > common.weight);
    let Connection::Styles { styles } = rare.connection else {
        unreachable!()
    };
    assert_eq!(
        styles
            .iter()
            .map(|style| style.name.as_deref().unwrap())
            .collect::<Vec<_>>(),
        ["Made-Up Rare"]
    );
}

#[test]
fn a_seed_s_plays_raise_its_connections_and_a_seed_without_plays_still_counts() {
    let catalog = catalog("plays");
    let weights = |plays| -> (f64, f64) {
        let scored = scored(
            &catalog,
            vec![
                album("Cora Seed", "Seed Album", Some(2001), plays),
                seed(1003),
            ],
        );
        // Dee's credit, from seed 2001, and DJ Ann's alias, from seed 1003.
        let dee = reasons(&scored, Candidate::Release(2003))[0].weight;
        let alias = reasons(&scored, Candidate::Release(2008))[0].weight;
        (dee, alias)
    };
    let (dee_unplayed, alias_unplayed) = weights(0);
    let (dee_played, alias_played) = weights(50);
    assert!(
        dee_played > dee_unplayed,
        "{dee_played} against {dee_unplayed}"
    );
    assert!(
        alias_played < alias_unplayed && alias_played > 0.0,
        "{alias_played} against {alias_unplayed}"
    );
}

#[test]
fn a_candidate_the_library_has_is_never_scored() {
    let catalog = catalog("library");
    // A seed is the library's own release: 1003 is by Ann, and connects to itself.
    assert!(
        find(
            &scored(&catalog, vec![seed(1003)]),
            Candidate::Release(1003)
        )
        .is_none()
    );
    // Master 100 connects to Ann's 1003 by the band, and to 1002 by its artist, but 1002 is
    // one of its pressings.
    assert!(find(&scored(&catalog, vec![seed(1003)]), Candidate::Master(100)).is_some());
    assert!(find(&scored(&catalog, vec![seed(1002)]), Candidate::Master(100)).is_none());
    // An untagged album with 2002's artist and title.
    assert!(
        find(
            &scored(&catalog, vec![seed(2001)]),
            Candidate::Release(2002)
        )
        .is_some()
    );
    let named_too = vec![
        seed(2001),
        album("Cora Seed", "Second Album (Remastered)", None, 0),
    ];
    assert!(find(&scored(&catalog, named_too), Candidate::Release(2002)).is_none());
}

#[test]
fn a_candidate_s_entry_has_its_master_s_year_or_its_release_s() {
    let catalog = catalog("entry-year");
    let profile = Profile {
        albums: vec![seed(2001), seed(1003)],
    };
    let seeds = Seeds::read(&profile, &catalog).unwrap();
    let connections = Connections::read(&catalog, &seeds).unwrap();
    let year = |candidate| connections.entries[&candidate].year;
    // Master 100's year is 1999, its reissue 1002's 2004.
    assert_eq!(year(Candidate::Master(100)), Some(1999));
    assert_eq!(year(Candidate::Release(2003)), Some(2001));
    // 2002 has no release date.
    assert_eq!(year(Candidate::Release(2002)), None);
}

#[test]
fn a_compilation_of_various_artists_is_never_scored() {
    // 2014 is Various's, on Seed Records in 2001, within the seed's 3 years, as 2003 is.
    let scored = scored(&catalog("various"), vec![seed(2001)]);
    assert!(find(&scored, Candidate::Release(2003)).is_some());
    assert!(find(&scored, Candidate::Release(2014)).is_none());
}

#[test]
fn a_candidate_keeps_its_strongest_connections_and_the_best_scores_1() {
    let scored = scored(&catalog("reasons"), vec![seed(2001)]);
    // 2003 has Dee's credit, Cora's as its singer, Seed Records and Made-Up Common.
    let best = &scored[0];
    assert_eq!(best.candidate, Candidate::Release(2003));
    assert!(same(best.score, 1.0));
    let weights: Vec<f64> = best.reasons.iter().map(|reason| reason.weight).collect();
    // Cora's credit: on 4 releases, 3 / log2(6); the label on 3: 2 / log2(5). The style's
    // 0.5 is the weakest and left out.
    let expected = [1.5, 3.0 / 6f64.log2(), 2.0 / 5f64.log2()];
    assert!(
        weights.iter().zip(expected).all(|(&a, b)| same(a, b)) && weights.len() == 3,
        "{:?}",
        best.reasons
    );
    assert!(matches!(
        &best.reasons[1].connection,
        Connection::Credit { artist, role, seed_roles, .. }
            if artist.id == 20 && role.as_deref() == Some("Vocals") && seed_roles.is_empty()
    ));
    // The total: the two credits, the second at a half, the label and the style.
    let total = 1.5 + expected[1] / 2.0 + expected[2] + 0.5;
    let second = find(&scored, Candidate::Release(2002)).unwrap();
    let rare = 0.25;
    assert!(same(second.score, (2.0 + rare) / total), "{second:?}");
}

#[test]
fn a_score_from_some_seeds_reads_only_their_connections() {
    let catalog = catalog("scope");
    let profile = Profile {
        albums: vec![seed(2001), seed(1003)],
    };
    let seeds = Seeds::read(&profile, &catalog).unwrap();
    let connections = Connections::read(&catalog, &seeds).unwrap();
    let library = Library::new(&profile, &catalog).unwrap();
    let only = |ids: &[u64]| {
        score::score(
            &seeds,
            &connections,
            &library,
            &Scope::Releases(ids.iter().copied().collect::<HashSet<u64>>()),
        )
    };
    let ann = only(&[1003]);
    assert!(find(&ann, Candidate::Release(2008)).is_some());
    assert!(find(&ann, Candidate::Release(2003)).is_none());
    let cora = only(&[2001]);
    assert!(find(&cora, Candidate::Release(2003)).is_some());
    assert!(find(&cora, Candidate::Release(2008)).is_none());
    assert!(only(&[]).is_empty());
}

/// The budget of a full scoring (plan, phase 4): under 10 minutes and 16 GB at 19.5M
/// releases. Measured on 1M generated releases with 4,000 seeds, about the library's
/// tagged albums, spread over the catalog; projected in ADR 0026. Run with
/// `cargo test --release -p features --test score -- --ignored budget --nocapture` under
/// `/usr/bin/time -l` for the peak memory.
#[test]
#[ignore = "a benchmark: generates and imports 1M releases"]
fn budget_on_1m_generated_releases() {
    use std::time::Instant;
    const RELEASES: u64 = 1_000_000;
    const SEEDS: u64 = 4000;
    let started = Instant::now();
    let catalog = catalog_support::imported("score", "budget", |dumps| {
        generated::write_set(dumps, RELEASES);
    });
    println!("generated and imported in {:?}", started.elapsed());
    // The generator's release `id` is by artist `id % (RELEASES / 2) + 1`.
    let albums = (0..SEEDS)
        .map(|n| {
            let id = n * (RELEASES / SEEDS) + 1;
            let artist = format!("Artist {}", id % (RELEASES / 2) + 1);
            let title = format!("Generated Release Number {id}");
            album(&artist, &title, Some(id), n % 7 * 20)
        })
        .collect();
    let profile = Profile { albums };
    // The pass alone, which grows with the catalog; the rest grows with what connects.
    let started = Instant::now();
    let mut releases = 0u64;
    catalog.scan(|_| releases += 1).unwrap();
    println!("{releases} releases scanned in {:?}", started.elapsed());
    let started = Instant::now();
    let seeds = Seeds::read(&profile, &catalog).unwrap();
    let seeds_took = started.elapsed();
    let connections = Connections::read(&catalog, &seeds).unwrap();
    let connections_took = started.elapsed().saturating_sub(seeds_took);
    let library = Library::new(&profile, &catalog).unwrap();
    let scoring = Instant::now();
    let scored = score::score(&seeds, &connections, &library, &Scope::Library);
    let score_took = scoring.elapsed();
    println!(
        "{} seeds read in {seeds_took:?}; {} candidates connected in {connections_took:?}; \
         {} scored in {score_took:?}; {:?} in all",
        seeds.seeds.len(),
        connections.entries.len(),
        scored.len(),
        started.elapsed()
    );
    assert_eq!(seeds.seeds.len(), 4000);
    assert!(!scored.is_empty());
}
