//! The catalog score (recommendations.md, Catalog connections), ported from the prototype's
//! rules and weights: each candidate's connections to the seeds in scope, each kind summed
//! with diminishing shares, the total scaled to the best candidate's, and the strongest
//! connections kept as its reasons. Candidates the library has, by id or by name, and
//! compilations of Various Artists are never scored (ADR 0026).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use catalog::discogs::Entry;

use crate::candidate::Candidate;
use crate::connection::{Connections, Link};
use crate::library::Library;
use crate::seed::{Relation, Role, Seeds, YEARS};

/// The weight of a candidate by a seed's artist, at the most-weighted seed artist's.
const ARTIST: f64 = 2.0;
/// The weight of a credit, before its roles, how many seeds share it and its rarity.
const CREDIT: f64 = 3.0;
/// The weight of a label family, before how many seeds share it and its rarity.
const LABEL: f64 = 2.0;
/// The weight of an alias, group or member of a seed's artist, at the most-weighted seed
/// artist's.
const RELATED: f64 = 1.5;
/// The weight of a candidate whose every style is the seeds' whole share.
const STYLE: f64 = 0.5;
/// How many of its connections a candidate keeps as reasons.
const REASONS: usize = 3;
/// How many of the seeds' styles a style reason names.
const STYLES_NAMED: usize = 3;
/// Discogs's "Various", the artist of compilations.
pub(crate) const VARIOUS: u64 = 194;

/// Which seeds a score starts from.
#[derive(Debug, Clone)]
pub enum Scope {
    /// Every seed.
    Library,
    /// The seeds tagged with these releases: one album, an artist's, or what the
    /// evaluation keeps.
    Releases(HashSet<u64>),
}

/// A candidate, its score and its reasons.
#[derive(Debug, Clone)]
pub struct Scored {
    /// The candidate.
    pub candidate: Candidate,
    /// Its catalog score, from 0 to 1 for the best candidate.
    pub score: f64,
    /// Its strongest connections, strongest first.
    pub reasons: Vec<Reason>,
}

/// One connection of a candidate and its weight, before the scaling.
#[derive(Debug, Clone, PartialEq)]
pub struct Reason {
    /// What the connection adds to the candidate's score, before its kind's diminishing
    /// share.
    pub weight: f64,
    /// What connects.
    pub connection: Connection,
}

/// What connects a candidate to the seeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Connection {
    /// By an artist of the seeds.
    Artist {
        /// The artist.
        artist: Named,
        /// The seeds it is on.
        albums: usize,
    },
    /// A person on the seeds is credited on the candidate, or is its artist.
    Credit {
        /// The person.
        artist: Named,
        /// The person's role on the candidate, `None` when the candidate's artist.
        role: Option<String>,
        /// The person's credited roles on the seeds, strongest first; none when they are
        /// on the seeds only as an artist.
        seed_roles: Vec<String>,
        /// The seeds the person is on.
        albums: usize,
    },
    /// On a label of the seeds' families within `YEARS` years of a seed on it.
    Label {
        /// The candidate's label.
        label: Named,
        /// The seeds' label, when the candidate's is its sublabel or parent.
        via: Option<Named>,
        /// The seeds on the family within the years.
        albums: usize,
    },
    /// By an alias, group or member of a seed's artist.
    Related {
        /// The candidate's artist.
        artist: Named,
        /// How it is related.
        relation: Relation,
        /// The seed's artist.
        of: Named,
    },
    /// Styles the seeds have, weighted by their rarity.
    Styles {
        /// The candidate's styles the seeds have, the most shared first.
        styles: Vec<Named>,
    },
}

/// An artist, label or style of the catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Named {
    /// Its Discogs id, or the catalog's id for a style.
    pub id: u64,
    /// Its name, `None` when the catalog has no row for it.
    pub name: Option<String>,
}

/// The kinds of connection, each summed apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Artist,
    Credit,
    Label,
    Related,
    Styles,
}

impl Connection {
    fn kind(&self) -> Kind {
        match self {
            Self::Artist { .. } => Kind::Artist,
            Self::Credit { .. } => Kind::Credit,
            Self::Label { .. } => Kind::Label,
            Self::Related { .. } => Kind::Related,
            Self::Styles { .. } => Kind::Styles,
        }
    }
}

/// The candidates `connections` found, scored from the seeds in `scope`, the best first,
/// leaving out what `library` has and compilations of Various Artists. A candidate with no
/// connection to a seed in scope, styles aside, isn't scored.
#[must_use]
pub fn score(
    seeds: &Seeds,
    connections: &Connections,
    library: &Library,
    scope: &Scope,
) -> Vec<Scored> {
    let Some(context) = Context::new(seeds, connections, scope) else {
        return Vec::new();
    };
    let mut scored = Vec::new();
    for (&candidate, links) in &connections.links {
        let entry = &connections.entries[&candidate];
        if library.has_id(candidate) || is_various(entry) || has_name(library, entry) {
            continue;
        }
        let mut reasons = context.reasons(links, entry);
        if reasons.is_empty() {
            continue;
        }
        let total = combined(&reasons);
        reasons.sort_by(|a, b| b.weight.total_cmp(&a.weight));
        reasons.truncate(REASONS);
        scored.push(Scored {
            candidate,
            score: total,
            reasons,
        });
    }
    let best = scored.iter().map(|s| s.score).fold(0.0, f64::max);
    for candidate in &mut scored {
        candidate.score /= best;
    }
    scored.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| order(a.candidate).cmp(&order(b.candidate)))
    });
    scored
}

/// A candidate's place among equal scores, so a sort doesn't depend on a map's order.
pub(crate) fn order(candidate: Candidate) -> (u8, u64) {
    match candidate {
        Candidate::Master(id) => (0, id),
        Candidate::Release(id) => (1, id),
    }
}

/// Whether every artist of the candidate is Various Artists.
pub(crate) fn is_various(entry: &Entry) -> bool {
    entry.artists.iter().all(|&(id, _)| id == VARIOUS)
}

/// Whether the candidate is a library album by its folded artist and title.
pub(crate) fn has_name(library: &Library, entry: &Entry) -> bool {
    let artists: Vec<&str> = entry
        .artists
        .iter()
        .filter_map(|(_, name)| name.as_deref())
        .collect();
    library.has_name(&artists, &entry.title)
}

/// Each kind's connections summed, the strongest whole and the next at a half, a third and
/// so on, so many weak connections of one kind don't outweigh one strong one.
fn combined(reasons: &[Reason]) -> f64 {
    let mut kinds: BTreeMap<Kind, Vec<f64>> = BTreeMap::new();
    for reason in reasons {
        kinds
            .entry(reason.connection.kind())
            .or_default()
            .push(reason.weight);
    }
    kinds
        .into_values()
        .map(|mut weights| {
            weights.sort_by(|a, b| b.total_cmp(a));
            weights
                .iter()
                .zip(1u32..)
                .map(|(weight, place)| weight / f64::from(place))
                .sum::<f64>()
        })
        .sum()
}

/// What a score reads of the seeds in scope.
struct Context<'a> {
    seeds: &'a Seeds,
    /// Each seed's weight, scaled to a mean of 1 over the seeds in scope, 0 out of it.
    weights: Vec<f64>,
    /// Each artist of a seed in scope, with the weights of its seeds summed.
    artist_weights: HashMap<u64, f64>,
    /// The greatest of `artist_weights`.
    max_artist: f64,
    /// Each style's share of the weight of the seeds in scope that have styles.
    shares: HashMap<u32, f64>,
    /// Each style's rarity: the log of all styles' releases over its own.
    rarity: HashMap<u32, f64>,
}

impl<'a> Context<'a> {
    /// The context of `scope`, or `None` when no seed is in it.
    fn new(seeds: &'a Seeds, connections: &Connections, scope: &Scope) -> Option<Self> {
        let in_scope: Vec<bool> = seeds
            .seeds
            .iter()
            .map(|seed| match scope {
                Scope::Library => true,
                Scope::Releases(ids) => ids.contains(&seed.release_id),
            })
            .collect();
        let count = in_scope.iter().filter(|&&in_scope| in_scope).count();
        if count == 0 {
            return None;
        }
        let total: f64 = seeds
            .seeds
            .iter()
            .zip(&in_scope)
            .filter(|&(_, &in_scope)| in_scope)
            .map(|(seed, _)| seed.weight)
            .sum();
        #[expect(
            clippy::cast_precision_loss,
            reason = "a library's seeds, far below 2^52"
        )]
        let mean = total / count as f64;
        let weights: Vec<f64> = seeds
            .seeds
            .iter()
            .zip(&in_scope)
            .map(|(seed, &in_scope)| if in_scope { seed.weight / mean } else { 0.0 })
            .collect();
        let mut artist_weights: HashMap<u64, f64> = HashMap::new();
        let mut with_styles = 0.0;
        let mut style_weights: HashMap<u32, f64> = HashMap::new();
        for (seed, &weight) in seeds.seeds.iter().zip(&weights) {
            if weight == 0.0 {
                continue;
            }
            let artists: BTreeSet<u64> = seed.artists.iter().copied().collect();
            for artist in artists {
                *artist_weights.entry(artist).or_default() += weight;
            }
            let styles: BTreeSet<u32> = seed.styles.iter().copied().collect();
            if !styles.is_empty() {
                with_styles += weight;
            }
            for style in styles {
                *style_weights.entry(style).or_default() += weight;
            }
        }
        let max_artist = artist_weights.values().copied().fold(0.0, f64::max);
        let shares = style_weights
            .into_iter()
            .map(|(style, weight)| (style, weight / with_styles))
            .collect();
        #[expect(
            clippy::cast_precision_loss,
            reason = "release counts far below 2^52; the log needs no more"
        )]
        let rarity = {
            let all = connections.style_releases.values().sum::<u64>() as f64;
            connections
                .style_releases
                .iter()
                .map(|(&style, &releases)| (style, (all / releases as f64).ln()))
                .collect()
        };
        Some(Self {
            seeds,
            weights,
            artist_weights,
            max_artist,
            shares,
            rarity,
        })
    }

    /// A candidate's connections to the seeds in scope, from its links and its entry.
    fn reasons(&self, links: &HashMap<Link, f64>, entry: &Entry) -> Vec<Reason> {
        let mut artists = BTreeSet::new();
        let mut credits: BTreeMap<u64, Vec<u32>> = BTreeMap::new();
        let mut labels: BTreeMap<u64, Vec<u16>> = BTreeMap::new();
        let mut related = BTreeSet::new();
        for link in links.keys() {
            match *link {
                Link::Artist(artist) => {
                    artists.insert(artist);
                }
                Link::Credit(artist, role) => credits.entry(artist).or_default().push(role),
                Link::Label(label, year) => labels.entry(label).or_default().push(year),
                Link::Related(artist) => {
                    related.insert(artist);
                }
            }
        }
        let people: BTreeSet<u64> = artists.iter().chain(credits.keys()).copied().collect();
        let mut reasons: Vec<Reason> = people
            .into_iter()
            .filter_map(|person| self.person(person, &artists, credits.get(&person)))
            .collect();
        reasons.extend(
            labels
                .iter()
                .filter_map(|(&label, years)| self.label(label, years)),
        );
        reasons.extend(self.related(&related));
        if !reasons.is_empty()
            && let Some(styles) = self.styles(&entry.styles)
        {
            reasons.push(styles);
        }
        reasons
    }

    /// A person on the seeds in scope: by the artist when they are the candidate's artist
    /// and a seed's, a credit otherwise.
    fn person(
        &self,
        person: u64,
        artists: &BTreeSet<u64>,
        roles: Option<&Vec<u32>>,
    ) -> Option<Reason> {
        let parts: Vec<(usize, Role)> = self.seeds.people[&person]
            .iter()
            .copied()
            .filter(|&(seed, _)| self.weights[seed] > 0.0)
            .collect();
        if parts.is_empty() {
            return None;
        }
        let albums: BTreeSet<usize> = parts.iter().map(|&(seed, _)| seed).collect();
        let is_artist = artists.contains(&person);
        if is_artist && parts.iter().any(|&(_, role)| role == Role::Artist) {
            return Some(Reason {
                weight: ARTIST * self.artist_weights[&person] / self.max_artist,
                connection: Connection::Artist {
                    artist: self.artist(person),
                    albums: albums.len(),
                },
            });
        }
        // The candidate's artist counts as a role of weight 1, as the prototype's "artist".
        let (role_weight, role) = if is_artist {
            (1.0, None)
        } else {
            let best = roles
                .into_iter()
                .flatten()
                .copied()
                .max_by(|a, b| {
                    self.role_weight(*a)
                        .total_cmp(&self.role_weight(*b))
                        .then(b.cmp(a))
                })
                .unwrap_or_else(|| panic!("artist {person} links with no role"));
            (self.role_weight(best), Some(best))
        };
        let mut seed_roles: Vec<u32> = parts
            .iter()
            .filter_map(|&(_, role)| match role {
                Role::Artist => None,
                Role::Credit(role) => Some(role),
            })
            .collect::<BTreeSet<u32>>()
            .into_iter()
            .collect();
        seed_roles.sort_by(|a, b| self.role_weight(*b).total_cmp(&self.role_weight(*a)));
        let seed_weight = parts
            .iter()
            .map(|&(_, role)| match role {
                Role::Artist => 1.0,
                Role::Credit(role) => self.role_weight(role),
            })
            .fold(0.0, f64::max);
        let shared: f64 = albums.iter().map(|&seed| self.weights[seed]).sum();
        let weight = role_weight
            * seed_weight
            * shared.sqrt()
            * rarity(self.seeds.artist_releases[&person])
            * CREDIT;
        Some(Reason {
            weight,
            connection: Connection::Credit {
                artist: self.artist(person),
                role: role.map(|role| self.role_name(role)),
                seed_roles: seed_roles
                    .into_iter()
                    .map(|role| self.role_name(role))
                    .collect(),
                albums: albums.len(),
            },
        })
    }

    /// A label of the seeds' families, when the candidate is on it within `YEARS` years of
    /// a seed in scope on the family.
    fn label(&self, label: u64, years: &[u16]) -> Option<Reason> {
        let near: Vec<_> = self.seeds.families[&label]
            .iter()
            .filter(|on| {
                self.weights[on.seed] > 0.0
                    && years.iter().any(|year| year.abs_diff(on.year) <= YEARS)
            })
            .collect();
        if near.is_empty() {
            return None;
        }
        let albums: BTreeSet<usize> = near.iter().map(|on| on.seed).collect();
        let shared: f64 = albums.iter().map(|&seed| self.weights[seed]).sum();
        let via = if near.iter().any(|on| on.label == label) {
            None
        } else {
            near.iter()
                .map(|on| on.label)
                .min()
                .map(|id| self.label_named(id))
        };
        Some(Reason {
            weight: shared.sqrt() * rarity(self.seeds.label_releases[&label]) * LABEL,
            connection: Connection::Label {
                label: self.label_named(label),
                via,
                albums: albums.len(),
            },
        })
    }

    /// The aliases, groups and members of the seeds' artists in scope among the
    /// candidate's artists: one reason for each seed artist, by the lowest related id.
    fn related(&self, related: &BTreeSet<u64>) -> Vec<Reason> {
        let mut by_seed_artist: BTreeMap<u64, (u64, Relation)> = BTreeMap::new();
        for &artist in related {
            for &(relation, seed_artist) in &self.seeds.related[&artist] {
                if self.artist_weights.contains_key(&seed_artist) {
                    by_seed_artist
                        .entry(seed_artist)
                        .or_insert((artist, relation));
                }
            }
        }
        by_seed_artist
            .into_iter()
            .map(|(seed_artist, (artist, relation))| Reason {
                weight: RELATED * self.artist_weights[&seed_artist] / self.max_artist,
                connection: Connection::Related {
                    artist: self.artist(artist),
                    relation,
                    of: self.artist(seed_artist),
                },
            })
            .collect()
    }

    /// The candidate's styles the seeds in scope have, each counted by the root of its
    /// share and weighted by its rarity, against all of the candidate's styles. A style no
    /// release has, which only a master can, has no rarity and is left out.
    fn styles(&self, styles: &[u32]) -> Option<Reason> {
        let styles: BTreeSet<u32> = styles.iter().copied().collect();
        let rated: Vec<(u32, f64, f64)> = styles
            .into_iter()
            .filter_map(|style| {
                let rarity = *self.rarity.get(&style)?;
                Some((
                    style,
                    self.shares.get(&style).copied().unwrap_or(0.0),
                    rarity,
                ))
            })
            .collect();
        let all: f64 = rated.iter().map(|&(_, _, rarity)| rarity).sum();
        let shared: f64 = rated
            .iter()
            .map(|&(_, share, rarity)| share.sqrt() * rarity)
            .sum();
        if shared <= 0.0 {
            return None;
        }
        let mut liked: Vec<(u32, f64)> = rated
            .iter()
            .filter(|&&(_, share, _)| share > 0.0)
            .map(|&(style, share, _)| (style, share))
            .collect();
        liked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        Some(Reason {
            weight: STYLE * shared / all,
            connection: Connection::Styles {
                styles: liked
                    .into_iter()
                    .take(STYLES_NAMED)
                    .map(|(style, _)| Named {
                        id: u64::from(style),
                        name: self.seeds.style_names.get(&style).cloned(),
                    })
                    .collect(),
            },
        })
    }

    fn role_weight(&self, role: u32) -> f64 {
        self.seeds.role_weights[&role]
    }

    fn role_name(&self, role: u32) -> String {
        self.seeds.role_names[&role].clone()
    }

    fn artist(&self, id: u64) -> Named {
        Named {
            id,
            name: self.seeds.artist_names.get(&id).cloned(),
        }
    }

    fn label_named(&self, id: u64) -> Named {
        Named {
            id,
            name: self.seeds.label_names.get(&id).cloned(),
        }
    }
}

/// The rarity of an artist or label on `releases` releases.
fn rarity(releases: u64) -> f64 {
    #[expect(
        clippy::cast_precision_loss,
        reason = "release counts far below 2^52; the log needs no more"
    )]
    let releases = releases as f64;
    1.0 / (2.0 + releases).log2()
}

#[cfg(test)]
mod tests {
    use super::{Connection, Named, Reason, combined};

    fn credit(id: u64, weight: f64) -> Reason {
        Reason {
            weight,
            connection: Connection::Credit {
                artist: Named { id, name: None },
                role: None,
                seed_roles: Vec::new(),
                albums: 1,
            },
        }
    }

    #[test]
    fn many_weak_connections_of_a_kind_don_t_beat_one_strong_one() {
        let strong = combined(&[credit(1, 1.0)]);
        let weak: Vec<Reason> = (2..6).map(|id| credit(id, 0.3)).collect();
        // Summed whole they would be 1.2.
        let weak = combined(&weak);
        assert!(weak < strong, "{weak} against {strong}");
        assert!((weak - 0.3 * (1.0 + 1.0 / 2.0 + 1.0 / 3.0 + 1.0 / 4.0)).abs() < 1e-12);
    }

    #[test]
    fn each_kind_is_summed_apart() {
        let label = Reason {
            weight: 0.5,
            connection: Connection::Label {
                label: Named { id: 9, name: None },
                via: None,
                albums: 1,
            },
        };
        let total = combined(&[credit(1, 1.0), credit(2, 1.0), label]);
        // The second credit at a half; the label, the first of its kind, whole.
        assert!((total - 2.0).abs() < 1e-12, "{total}");
    }
}
