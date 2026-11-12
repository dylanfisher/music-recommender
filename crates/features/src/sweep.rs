//! The popularity sweep (ADR 0030): list requests that give the owners and wants of up to 100
//! releases or masters each, queued after the library and before the candidates' ratings.
//! Its artists are the library's, their aliases, groups and members, and the people credited
//! most on the library; its masters are the list's best with more than one pressing, whose
//! versions say which pressing is the most owned.

use std::cmp::Ordering;
use std::collections::HashMap;

use catalog::discogs::Catalog;
use catalog::feedback::Kind;
use catalog::score::{Filter, Scores};

use crate::score::VARIOUS;
use crate::seed::{Role, Seeds};
use crate::sync::SyncError;

/// What an artist of the seeds, one related to them and one credited on them weigh, times
/// the weight of the seeds they come from: the score's artist and related weights (ADR
/// 0026), and a credit below both, so the library's own artists are swept first.
const ARTIST: f64 = 2.0;
const RELATED: f64 = 1.5;
const CREDIT: f64 = 1.0;

/// How many of the people credited on the seeds are swept, the strongest: most of a
/// library's credits are on one album each.
const CREDITS: usize = 200;

/// How many of the list's best masters are looked at for their pressings: about the first
/// five batches of ratings.
const MASTERS: usize = 5000;

/// A master with at least this many releases has its versions swept: with one, its only
/// release is its representative already.
const PRESSINGS: u64 = 2;

/// The artists to sweep, each with its priority, the highest first and equal ones by id:
/// each of the seeds' artists at `ARTIST` times the weight of its seeds, each artist related
/// to them at `RELATED` times the weight of the seeds of the artists it is related to, and
/// the `CREDITS` people with the most credits on the seeds at `CREDIT` times the weight of
/// the seeds they are on, each seed at their strongest role's weight there. An artist found
/// several ways takes its highest. Discogs's Various is left out.
///
/// # Errors
///
/// When a credit's role isn't among the catalog's, which no import writes.
pub(crate) fn artists(seeds: &Seeds, catalog: &Catalog) -> Result<Vec<(u64, f64)>, SyncError> {
    let mut weight: HashMap<u64, f64> = HashMap::new();
    for seed in &seeds.seeds {
        for &artist in &seed.artists {
            *weight.entry(artist).or_default() += seed.weight;
        }
    }
    let mut priority: HashMap<u64, f64> = HashMap::new();
    let mut raise = |id: u64, value: f64| {
        let kept = priority.entry(id).or_default();
        *kept = kept.max(value);
    };
    for (&artist, &w) in &weight {
        raise(artist, ARTIST * w);
    }
    for (&artist, ways) in &seeds.related {
        let w: f64 = ways.iter().filter_map(|(_, of)| weight.get(of)).sum();
        raise(artist, RELATED * w);
    }
    let mut credits = Vec::new();
    for (&person, part) in &seeds.people {
        let mut on: HashMap<usize, f64> = HashMap::new();
        for &(seed, role) in &part.parts {
            let Role::Credit(role) = role else {
                continue;
            };
            let role_weight = seeds
                .roles
                .get(&role)
                .ok_or_else(|| catalog.inconsistent(format!("a credit's role {role} has no name")))?
                .weight;
            let kept = on.entry(seed).or_default();
            *kept = kept.max(role_weight);
        }
        let w: f64 = on
            .iter()
            .map(|(&seed, role_weight)| seeds.seeds[seed].weight * role_weight)
            .sum();
        if w > 0.0 {
            credits.push((person, CREDIT * w));
        }
    }
    best_first(&mut credits);
    credits.truncate(CREDITS);
    for (person, value) in credits {
        raise(person, value);
    }
    priority.remove(&VARIOUS);
    let mut artists: Vec<(u64, f64)> = priority.into_iter().collect();
    best_first(&mut artists);
    Ok(artists)
}

/// The masters to sweep, best first: those among the list's first `MASTERS` masters with at
/// least `PRESSINGS` releases in the catalog, each with its score as its priority.
///
/// # Errors
///
/// When the scores or the catalog fail.
pub(crate) fn masters(scores: &Scores, catalog: &Catalog) -> Result<Vec<(u64, f64)>, SyncError> {
    let mut masters = Vec::new();
    let mut looked = 0;
    let mut failed = None;
    scores.walk(&Filter::default(), false, &mut |head| {
        if head.kind != Kind::Master {
            return true;
        }
        looked += 1;
        match catalog.master_releases(head.id, PRESSINGS) {
            Ok(releases) if releases >= PRESSINGS => masters.push((head.id, head.score)),
            Ok(_) => {}
            Err(error) => {
                failed = Some(error);
                return false;
            }
        }
        looked < MASTERS
    })?;
    match failed {
        Some(error) => Err(error.into()),
        None => Ok(masters),
    }
}

/// Sorts by priority, the highest first, and equal ones by id, so the queue's order is the
/// same each time.
fn best_first(items: &mut [(u64, f64)]) {
    items.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
}
