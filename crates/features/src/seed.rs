//! The library's side of the catalog connections (recommendations.md, Catalog connections):
//! the seeds, which are the profile's albums tagged with a release the catalog has, each
//! weighted by its plays, loves and rating, and what they share with other releases: the
//! people on them, their labels' families by year, their artists' aliases, groups and
//! members, and their styles (ADR 0026). Read once from the catalog; a score of any subset
//! of the seeds reads it again from memory.

use std::collections::{HashMap, HashSet};

use catalog::discogs::{Catalog, CatalogError};
pub use catalog::score::Relation;

use crate::profile::Profile;

// A change to a constant here that changes a stored list needs a bump of `score::VERSION`.
/// How many years a candidate's release may be from a seed's on the same label family.
pub(crate) const YEARS: u16 = 3;

/// A person on more releases than this says little, and is left out of the connections;
/// for a seed's artist that also drops their other albums (the prototype's cut).
const MAX_PERSON_RELEASES: u64 = 3000;

/// A label on more releases than this says little, and is left out of the families.
const MAX_LABEL_RELEASES: u64 = 20_000;

/// A seed's label whose name has this, ignoring case, is Discogs's placeholder for none.
pub(crate) const NOT_ON_LABEL: &str = "not on label";

/// The weight of a credit in a weak role, against 1 for any other.
const WEAK_ROLE: f64 = 0.2;

/// The roles that say little about the music, matched inside a role's lowercased name.
const WEAK_ROLES: [&str; 41] = [
    "master",
    "lacquer",
    "cut by",
    "design",
    "artwork",
    "photo",
    "layout",
    "liner",
    "art direction",
    "cover",
    "graphic",
    "copyright",
    "manufactured",
    "distributed",
    "pressed",
    "management",
    "a&r",
    "booking",
    "legal",
    "coordinator",
    "transferred",
    "illustration",
    "typography",
    "translated",
    "executive",
    "film",
    "video",
    "director",
    "technician",
    "authoring",
    "project",
    "supervised",
    "notes",
    "concept",
    "promotion",
    "marketing",
    "lyrics translated",
    "other",
    "photograph",
    "logo",
    "sleeve",
];

/// The plays a loved track counts as in a seed's weight.
const LOVED_PLAYS: u64 = 10;

/// How much a star of rating above or below 3 raises or lowers a seed's weight.
const RATING_STEP: f64 = 0.2;

/// A person's part on a seed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Role {
    /// One of the release's artists.
    Artist,
    /// A credit, by its role's id.
    Credit(u32),
}

/// A library album the connections start from.
#[derive(Debug)]
pub struct Seed {
    /// The release it is tagged with.
    pub release_id: u64,
    /// Its weight from plays, loves and rating, before a score scales the seeds' weights
    /// to a mean of 1.
    pub weight: f64,
    /// The release's artists.
    pub artists: Vec<u64>,
    /// The release's styles.
    pub(crate) styles: Vec<u32>,
}

/// A seed on a label family: the label the seed is on, the seed and its year.
#[derive(Debug, Clone, Copy)]
pub(crate) struct OnLabel {
    pub(crate) label: u64,
    pub(crate) seed: usize,
    pub(crate) year: u16,
}

/// A person on the seeds.
#[derive(Debug)]
pub(crate) struct Person {
    /// Each seed the person is on, with their part.
    pub(crate) parts: Vec<(usize, Role)>,
    /// The person's releases: exact, since a person over the cap is left out.
    pub(crate) releases: u64,
}

/// A label of a seed's family.
#[derive(Debug)]
pub(crate) struct Family {
    /// The seeds on the family.
    pub(crate) on: Vec<OnLabel>,
    /// The label's releases.
    pub(crate) releases: u64,
}

/// A credit's role.
#[derive(Debug)]
pub(crate) struct CreditRole {
    pub(crate) name: String,
    /// Its weight, from its name.
    pub(crate) weight: f64,
}

/// The seeds and what they share, read from the catalog.
#[derive(Debug, Default)]
pub struct Seeds {
    /// The seeds, in the profile's order of their first album.
    pub seeds: Vec<Seed>,
    /// Each person on a seed, at most `MAX_PERSON_RELEASES` releases.
    pub(crate) people: HashMap<u64, Person>,
    /// Each label of a seed's family, at most `MAX_LABEL_RELEASES` releases.
    pub(crate) families: HashMap<u64, Family>,
    /// Each artist related to a seed's artist and not one itself, with how and to whom.
    pub(crate) related: HashMap<u64, Vec<(Relation, u64)>>,
    /// Each role, by id.
    pub(crate) roles: HashMap<u32, CreditRole>,
    /// Each style's name, by id.
    pub(crate) style_names: HashMap<u32, String>,
    /// The names of the people, the related artists and the seeds' artists.
    pub(crate) artist_names: HashMap<u64, String>,
    /// The names of the labels in `families`.
    pub(crate) label_names: HashMap<u64, String>,
}

impl Seeds {
    /// The seeds of `profile`'s albums tagged with a release `catalog` has, and what they
    /// share. Two albums tagged with one release are one seed, at the greater weight.
    ///
    /// # Errors
    ///
    /// When a catalog read fails.
    pub fn read(profile: &Profile, catalog: &Catalog) -> Result<Self, CatalogError> {
        // Each tagged release once, in the order of its first album, at the greatest weight of
        // its albums. `places` holds only indexes of `order`, pushed beside them here.
        let mut places: HashMap<u64, usize> = HashMap::new();
        let mut order: Vec<(u64, f64)> = Vec::new();
        for album in &profile.albums {
            let Some(release_id) = album.discogs_release_id else {
                continue;
            };
            let weight = weight(album.plays, album.loved, album.rating);
            if let Some(&place) = places.get(&release_id) {
                order[place].1 = order[place].1.max(weight);
            } else {
                places.insert(release_id, order.len());
                order.push((release_id, weight));
            }
        }
        let mut seeds = Vec::new();
        let mut people: HashMap<u64, Vec<(usize, Role)>> = HashMap::new();
        let mut on_labels = Vec::new();
        for (release_id, weight) in order {
            let Some(links) = catalog.links(release_id)? else {
                continue;
            };
            let seed = seeds.len();
            for &artist in &links.artists {
                people.entry(artist).or_default().push((seed, Role::Artist));
            }
            for (artist, roles) in &links.credits {
                let parts = people.entry(*artist).or_default();
                parts.extend(roles.iter().map(|&role| (seed, Role::Credit(role))));
            }
            if let Some(year) = links.year {
                on_labels.extend(distinct(links.labels).into_iter().map(|label| OnLabel {
                    label,
                    seed,
                    year,
                }));
            }
            seeds.push(Seed {
                release_id,
                weight,
                artists: links.artists,
                styles: links.styles,
            });
        }
        let mut kept = HashMap::new();
        for (person, parts) in people {
            let releases = catalog.artist_releases(person, MAX_PERSON_RELEASES)?;
            if releases <= MAX_PERSON_RELEASES {
                kept.insert(person, Person { parts, releases });
            }
        }
        let people = kept;
        let (families, label_names) = families(catalog, &on_labels)?;
        let related = related(catalog, &seeds)?;
        let roles = catalog
            .role_names()?
            .into_iter()
            .map(|(id, name)| {
                let weight = role_weight(&name);
                (id, CreditRole { name, weight })
            })
            .collect();
        let named: HashSet<u64> = people
            .keys()
            .chain(related.keys())
            .chain(related.values().flatten().map(|(_, artist)| artist))
            .chain(seeds.iter().flat_map(|seed| &seed.artists))
            .copied()
            .collect();
        let mut artist_names = HashMap::new();
        for artist in named {
            if let Some(name) = catalog.artist_name(artist)? {
                artist_names.insert(artist, name);
            }
        }
        Ok(Self {
            seeds,
            people,
            families,
            related,
            roles,
            style_names: catalog.style_names()?,
            artist_names,
            label_names,
        })
    }
}

/// The families of the seeds' labels, with each label's releases, and the labels' names. A
/// label's family is itself, its parent and its sublabels, each left out when on more than
/// `MAX_LABEL_RELEASES` releases.
#[expect(clippy::type_complexity, reason = "two maps the caller keeps apart")]
fn families(
    catalog: &Catalog,
    on_labels: &[OnLabel],
) -> Result<(HashMap<u64, Family>, HashMap<u64, String>), CatalogError> {
    let mut families: HashMap<u64, Family> = HashMap::new();
    // Each label's releases, read once, the ones over the cap included.
    let mut label_releases = HashMap::new();
    let mut label_names = HashMap::new();
    if on_labels.is_empty() {
        return Ok((families, label_names));
    }
    let mut sublabels: HashMap<u64, Vec<u64>> = HashMap::new();
    for (label, parent) in catalog.sublabels()? {
        sublabels.entry(parent).or_default().push(label);
    }
    let mut labels: HashMap<u64, Option<(String, Option<u64>)>> = HashMap::new();
    for on_label in on_labels {
        let Some((name, parent)) = read_label(catalog, &mut labels, on_label.label)? else {
            continue;
        };
        if name.to_lowercase().contains(NOT_ON_LABEL) {
            continue;
        }
        label_names.insert(on_label.label, name);
        let family = std::iter::once(on_label.label).chain(parent).chain(
            sublabels
                .get(&on_label.label)
                .into_iter()
                .flatten()
                .copied(),
        );
        for member in family {
            let releases = if let Some(&releases) = label_releases.get(&member) {
                releases
            } else {
                let releases = catalog.label_releases(member, MAX_LABEL_RELEASES)?;
                label_releases.insert(member, releases);
                releases
            };
            if releases > MAX_LABEL_RELEASES {
                continue;
            }
            if let Some((name, _)) = read_label(catalog, &mut labels, member)? {
                label_names.insert(member, name);
            }
            families
                .entry(member)
                .or_insert_with(|| Family {
                    on: Vec::new(),
                    releases,
                })
                .on
                .push(*on_label);
        }
    }
    Ok((families, label_names))
}

/// Label `id`'s name and parent, read once into `labels`.
fn read_label(
    catalog: &Catalog,
    labels: &mut HashMap<u64, Option<(String, Option<u64>)>>,
    id: u64,
) -> Result<Option<(String, Option<u64>)>, CatalogError> {
    if let Some(label) = labels.get(&id) {
        return Ok(label.clone());
    }
    let label = catalog.label(id)?;
    labels.insert(id, label.clone());
    Ok(label)
}

/// The aliases, members and groups of the seeds' artists that aren't a seed's artist
/// themselves, each with how and to which seed artist it is related.
fn related(
    catalog: &Catalog,
    seeds: &[Seed],
) -> Result<HashMap<u64, Vec<(Relation, u64)>>, CatalogError> {
    let artists: HashSet<u64> = seeds.iter().flat_map(|seed| seed.artists.clone()).collect();
    let mut sorted: Vec<u64> = artists.iter().copied().collect();
    sorted.sort_unstable();
    let mut related: HashMap<u64, Vec<(Relation, u64)>> = HashMap::new();
    for artist in sorted {
        let relations = catalog.relations(artist)?;
        let all = [
            (Relation::Alias, relations.aliases),
            (Relation::Member, relations.members),
            (Relation::Group, relations.groups),
        ];
        for (relation, ids) in all {
            for id in ids {
                if !artists.contains(&id) {
                    let ways = related.entry(id).or_default();
                    if !ways.contains(&(relation, artist)) {
                        ways.push((relation, artist));
                    }
                }
            }
        }
    }
    Ok(related)
}

/// `ids` without a repeat, in the order each first appears.
fn distinct(mut ids: Vec<u64>) -> Vec<u64> {
    let mut seen = HashSet::new();
    ids.retain(|&id| seen.insert(id));
    ids
}

/// A credit's weight from its role's name: `WEAK_ROLE` for a role that says little about
/// the music, 1 for any other.
fn role_weight(name: &str) -> f64 {
    let name = name.to_lowercase();
    if WEAK_ROLES.iter().any(|weak| name.contains(weak)) {
        WEAK_ROLE
    } else {
        1.0
    }
}

/// A seed's weight from its album's plays, loved tracks and rating from 1 to 5: 1 for an
/// album in the library, plus the log of its plays with each loved track as
/// `LOVED_PLAYS` plays, so a seed without plays still counts and a much-played one counts
/// a few times more; then raised or lowered by `RATING_STEP` a star from 3.
fn weight(plays: u64, loved: u32, rating: Option<u8>) -> f64 {
    #[expect(
        clippy::cast_precision_loss,
        reason = "plays far below 2^52; the log needs no more"
    )]
    let plays = plays.saturating_add(u64::from(loved) * LOVED_PLAYS) as f64;
    let weight = 1.0 + plays.ln_1p();
    match rating {
        Some(rating) => weight * (1.0 + RATING_STEP * (f64::from(rating) - 3.0)),
        None => weight,
    }
}

#[cfg(test)]
mod tests {
    use super::{WEAK_ROLE, distinct, role_weight, weight};

    #[test]
    fn a_label_a_release_lists_twice_is_one_label_in_its_first_place() {
        // Discogs lists a label again for a second catalog number, not always next to the
        // first.
        assert_eq!(distinct(vec![5, 6, 5, 7, 6]), [5, 6, 7]);
        assert_eq!(distinct(vec![5, 5]), [5]);
        assert_eq!(distinct(Vec::new()), Vec::<u64>::new());
    }

    #[test]
    fn a_role_that_says_little_weighs_a_fifth() {
        assert!((role_weight("Lacquer Cut By") - WEAK_ROLE).abs() < f64::EPSILON);
        assert!((role_weight("Mastered By") - WEAK_ROLE).abs() < f64::EPSILON);
        assert!((role_weight("Producer") - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn a_seed_weighs_by_plays_loves_and_rating() {
        // An album without plays still counts.
        assert!((weight(0, 0, None) - 1.0).abs() < 1e-12);
        assert!((weight(100, 0, None) - (1.0 + 101f64.ln())).abs() < 1e-12);
        // A loved track counts as ten plays.
        assert!((weight(0, 2, None) - weight(20, 0, None)).abs() < 1e-12);
        // 5 stars is 1.4 times, 1 star 0.6 times, 3 stars as unrated.
        assert!((weight(0, 0, Some(5)) - 1.4).abs() < 1e-12);
        assert!((weight(0, 0, Some(1)) - 0.6).abs() < 1e-12);
        assert!((weight(9, 0, Some(3)) - weight(9, 0, None)).abs() < 1e-12);
    }
}
