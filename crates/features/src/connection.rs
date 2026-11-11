//! The candidates' side of the catalog connections (recommendations.md, Catalog
//! connections): one pass over every release in the catalog keeps each release that shares
//! a person, a label family within `YEARS` years or a related artist with the seeds, folded
//! into its candidate, and counts every style's releases for the styles' rarity (ADR 0026).
//! What connects is kept, not how much: the weights depend on which seeds a score starts
//! from, so one read serves the whole library's score, a seed's and the evaluation's.

use std::collections::HashMap;

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
#[derive(Debug)]
pub struct Connections {
    /// Each candidate's links, a link its pressings share kept once.
    pub(crate) links: HashMap<Candidate, HashMap<Link, f64>>,
    /// Each candidate's title, year, artists and styles: its master's, or its release's for a
    /// release without a master or a master the catalog doesn't have.
    pub entries: HashMap<Candidate, Entry>,
    /// Each style's releases, over the whole catalog.
    pub(crate) style_releases: HashMap<u32, u64>,
}

impl Connections {
    /// The candidates `catalog` connects to `seeds`, from one pass over its releases.
    ///
    /// A master the catalog doesn't have, which the masters dump can lack, is shown by its
    /// first connected release.
    ///
    /// # Errors
    ///
    /// When a catalog read fails.
    ///
    /// # Panics
    ///
    /// When the catalog has no entry for a release the pass read, or no name for a role
    /// on one: a build writes neither.
    pub fn read(catalog: &Catalog, seeds: &Seeds) -> Result<Self, CatalogError> {
        let families: HashMap<u64, Vec<u16>> = seeds
            .families
            .iter()
            .map(|(&label, on)| (label, on.iter().map(|on| on.year).collect()))
            .collect();
        let mut found = Vec::new();
        let mut first_release: HashMap<Candidate, u64> = HashMap::new();
        let mut style_releases: HashMap<u32, u64> = HashMap::new();
        catalog.scan(|links| {
            for &style in &links.styles {
                *style_releases.entry(style).or_default() += 1;
            }
            let before = found.len();
            let candidate = if links.master_id == 0 {
                Candidate::Release(links.id)
            } else {
                Candidate::Master(links.master_id)
            };
            for (link, weight) in release_links(links, seeds, &families) {
                found.push((candidate, link, weight));
            }
            if found.len() > before {
                first_release.entry(candidate).or_insert(links.id);
            }
        })?;
        let links = candidate::collapse(found);
        let mut entries = HashMap::with_capacity(links.len());
        for &candidate in links.keys() {
            let entry = match candidate {
                Candidate::Master(master_id) => match catalog.master_entry(master_id)? {
                    Some(entry) => Some(entry),
                    None => catalog.release_entry(first_release[&candidate])?,
                },
                Candidate::Release(release_id) => catalog.release_entry(release_id)?,
            };
            let entry = entry.unwrap_or_else(|| {
                panic!("{candidate:?} has no entry, though the pass read its release")
            });
            entries.insert(candidate, entry);
        }
        Ok(Self {
            links,
            entries,
            style_releases,
        })
    }
}

/// What connects one release to the seeds, each link with its weight for
/// [`candidate::collapse`]: a credit's role weight, 1 for the others.
fn release_links<'a>(
    links: &'a Links,
    seeds: &'a Seeds,
    families: &'a HashMap<u64, Vec<u16>>,
) -> impl Iterator<Item = (Link, f64)> + 'a {
    let artists = links.artists.iter().flat_map(|&artist| {
        let person = seeds
            .people
            .contains_key(&artist)
            .then_some((Link::Artist(artist), 1.0));
        let related = seeds
            .related
            .contains_key(&artist)
            .then_some((Link::Related(artist), 1.0));
        person.into_iter().chain(related)
    });
    let credits = links
        .credits
        .iter()
        .filter(|(artist, _)| seeds.people.contains_key(artist))
        .flat_map(|(artist, roles)| {
            roles.iter().map(|&role| {
                let weight =
                    seeds.role_weights.get(&role).copied().unwrap_or_else(|| {
                        panic!("role {role} of release {} has no name", links.id)
                    });
                (Link::Credit(*artist, role), weight)
            })
        });
    let labels = links.year.into_iter().flat_map(move |year| {
        links.labels.iter().filter_map(move |label| {
            let years = families.get(label)?;
            years
                .iter()
                .any(|&seed_year| seed_year.abs_diff(year) <= YEARS)
                .then_some((Link::Label(*label, year), 1.0))
        })
    });
    artists.chain(credits).chain(labels)
}
