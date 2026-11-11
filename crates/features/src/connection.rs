//! The candidates' side of the catalog connections (recommendations.md, Catalog
//! connections): one pass over every release in the catalog keeps each release that shares
//! a person, a label family within `YEARS` years or a related artist with the seeds, folded
//! into its candidate, and counts every style's releases for the styles' rarity (ADR 0026).
//! What connects is kept, not how much: the weights depend on which seeds a score starts
//! from, so one read serves the whole library's score, a seed's and the evaluation's.

use std::collections::{HashMap, HashSet};

use catalog::discogs::{Catalog, CatalogError, Entry, Links};

use crate::candidate::{self, Candidate};
use crate::seed::{Seeds, YEARS};

/// What connects a candidate to the seeds, found on one of its releases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Link {
    /// A person on the seeds is one of the release's artists.
    Artist(u64),
    /// A person on the seeds is credited on the release, in the role.
    Credit(u64, u32),
    /// The release is on a label of the seeds' families, in the year.
    Label(u64, u16),
    /// An artist related to a seed's artist is one of the release's artists.
    Related(u64),
}

/// The candidates connected to the seeds.
#[derive(Debug, Default)]
pub struct Connections {
    /// Each candidate connected, with what was found of it.
    pub candidates: HashMap<Candidate, Found>,
    /// Each style's releases, over the whole catalog.
    pub(crate) style_releases: HashMap<u32, u64>,
}

/// What connects a candidate to the seeds, and what the catalog says of it.
#[derive(Debug)]
pub struct Found {
    /// Its links, a link its pressings share kept once.
    pub(crate) links: HashSet<Link>,
    /// Its title, year, artists and styles: its master's, or its release's for a release
    /// without a master or a master the catalog doesn't have.
    pub entry: Entry,
}

impl Connections {
    /// The candidates `catalog` connects to `seeds`, from one pass over its releases. With no
    /// seeds nothing connects, and the pass, which reads every release, is skipped.
    ///
    /// A master the catalog doesn't have, which the masters dump can lack, is shown by its
    /// first release ([`Candidate::entry`]).
    ///
    /// # Errors
    ///
    /// When a catalog read fails, or the catalog has no entry for a release the pass read
    /// or no name for a role on one, which a build writes.
    pub fn read(catalog: &Catalog, seeds: &Seeds) -> Result<Self, CatalogError> {
        if seeds.seeds.is_empty() {
            return Ok(Self::default());
        }
        let families: HashMap<u64, Vec<u16>> = seeds
            .families
            .iter()
            .map(|(&label, family)| (label, family.on.iter().map(|on| on.year).collect()))
            .collect();
        let mut found = Vec::new();
        let mut problem = None;
        let mut style_releases: HashMap<u32, u64> = HashMap::new();
        catalog.scan(|links| {
            for &style in &links.styles {
                *style_releases.entry(style).or_default() += 1;
            }
            let candidate = Candidate::of(links.id, links.master_id);
            for link in release_links(links, seeds, &families) {
                match link {
                    Ok(link) => found.push((candidate, link)),
                    Err(unnamed) => {
                        problem.get_or_insert(unnamed);
                    }
                }
            }
        })?;
        if let Some(problem) = problem {
            return Err(catalog.inconsistent(problem));
        }
        let mut candidates = HashMap::new();
        for (candidate, links) in candidate::collapse(found) {
            let entry = candidate.entry(catalog)?.ok_or_else(|| {
                catalog.inconsistent(format!(
                    "{candidate:?} has no entry, though the pass read its release"
                ))
            })?;
            candidates.insert(candidate, Found { links, entry });
        }
        Ok(Self {
            candidates,
            style_releases,
        })
    }
}

/// What connects one release to the seeds, for [`candidate::collapse`]. A credit whose role
/// has no name is the error naming it.
fn release_links<'a>(
    links: &'a Links,
    seeds: &'a Seeds,
    families: &'a HashMap<u64, Vec<u16>>,
) -> impl Iterator<Item = Result<Link, String>> + 'a {
    let artists = links.artists.iter().flat_map(|&artist| {
        let person = seeds
            .people
            .contains_key(&artist)
            .then_some(Ok(Link::Artist(artist)));
        let related = seeds
            .related
            .contains_key(&artist)
            .then_some(Ok(Link::Related(artist)));
        person.into_iter().chain(related)
    });
    let credits = links
        .credits
        .iter()
        .filter(|(artist, _)| seeds.people.contains_key(artist))
        .flat_map(|(artist, roles)| {
            roles.iter().map(|&role| {
                if seeds.roles.contains_key(&role) {
                    Ok(Link::Credit(*artist, role))
                } else {
                    Err(format!("role {role} of release {} has no name", links.id))
                }
            })
        });
    let labels = links.year.into_iter().flat_map(move |year| {
        links.labels.iter().filter_map(move |label| {
            let years = families.get(label)?;
            years
                .iter()
                .any(|&seed_year| seed_year.abs_diff(year) <= YEARS)
                .then_some(Ok(Link::Label(*label, year)))
        })
    });
    artists.chain(credits).chain(labels)
}
