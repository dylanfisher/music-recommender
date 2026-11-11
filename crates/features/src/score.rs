//! The catalog score (recommendations.md, Catalog connections), ported from the prototype's
//! rules and weights: each candidate's connections to the seeds, each kind summed
//! with diminishing shares, the total scaled to the best candidate's, and the strongest
//! connections kept as its reasons. Candidates the library has, by id or by name, and
//! compilations of Various Artists are never scored (ADR 0026).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::{self, Display};
use std::hash::{BuildHasher, Hash};

use catalog::discogs::Entry;
use catalog::feedback::Kind;
pub use catalog::score::Named;
use catalog::score::ReasonKind;

use crate::candidate::Candidate;
use crate::connection::{Connections, Found, Link};
use crate::library::Library;
use crate::seed::{Relation, Role, Seeds, YEARS};

/// The version of the scoring code, stored with each list. Bump it whenever a weight or a rule
/// changes what a list holds, so that a redeploy rebuilds the lists the old code made
/// (ADR 0026). That covers this file, `connection.rs`, the seeds' rules and constants in
/// `seed.rs` (the weak roles, loved plays, rating step and release caps), the artist roll-up
/// and its shares in `artist.rs`, and the library's name matching and exclusions in
/// `library.rs` and its Various check in `query.rs`. `reason.rs` is not on the list: its
/// words are made when a list is read, not stored.
pub const VERSION: u32 = 2;

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

impl Connection {
    /// The kind of connection, each summed apart.
    pub(crate) fn kind(&self) -> ReasonKind {
        match self {
            Self::Artist { .. } => ReasonKind::Artist,
            Self::Credit { .. } => ReasonKind::Credit,
            Self::Label { .. } => ReasonKind::Label,
            Self::Related { .. } => ReasonKind::Related,
            Self::Styles { .. } => ReasonKind::Styles,
        }
    }
}

/// Why a score stopped. Seeds and connections read from one catalog never make one, so a
/// caller reports it as that catalog's inconsistency, where a panic would exit `serve` and
/// restart into the same panic (ADR 0026).
#[derive(Debug, PartialEq)]
pub enum ScoreError {
    /// A lookup in what the scoring reads found nothing: the seeds and the connections
    /// disagree about an id. Holds the problem, naming it.
    Missing(String),
    /// A candidate's total isn't finite and above 0, which the weights, all positive, can't
    /// make.
    NotAboveZero(Candidate, f64),
}

impl fmt::Display for ScoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(problem) => f.write_str(problem),
            Self::NotAboveZero(candidate, total) => write!(
                f,
                "{candidate:?} has links but a total of {total}, not a number above 0"
            ),
        }
    }
}

impl std::error::Error for ScoreError {}

/// The value of `key` in `map`, or the problem naming what is missing.
fn find<'m, K: Hash + Eq + Display, V, S: BuildHasher>(
    map: &'m HashMap<K, V, S>,
    key: &K,
    what: &str,
) -> Result<&'m V, ScoreError> {
    map.get(key).ok_or_else(|| {
        ScoreError::Missing(format!("no {what} for {key}, which the connections name"))
    })
}

/// The candidates `connections` found, scored from `seeds`, the best first, each with its
/// entry, leaving out what `library` has and compilations of Various Artists. A candidate
/// with no connection to a seed, styles aside, isn't scored.
///
/// # Errors
///
/// When the seeds and the connections disagree, a link to a person, label or role the seeds
/// don't have, or a total comes out not above 0.
pub fn score(
    seeds: &Seeds,
    connections: Connections,
    library: &Library,
) -> Result<Vec<(Scored, Entry)>, ScoreError> {
    let Some(context) = Context::new(seeds, &connections) else {
        return Ok(Vec::new());
    };
    let mut scored = Vec::new();
    for (candidate, Found { links, entry }) in connections.candidates {
        if library.has_id(candidate) || is_various(&entry) {
            continue;
        }
        let mut reasons = context.reasons(&links, &entry)?;
        // The name check folds and allocates, so it runs only for a candidate that would score.
        if reasons.is_empty() || has_name(library, &entry) {
            continue;
        }
        let total = combined(&reasons);
        reasons.sort_by(|a, b| b.weight.total_cmp(&a.weight));
        reasons.truncate(REASONS);
        let candidate = Scored {
            candidate,
            score: total,
            reasons,
        };
        scored.push((candidate, entry));
    }
    scale_to_best(&mut scored)?;
    scored.sort_by(|(a, _), (b, _)| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| order(a.candidate).cmp(&order(b.candidate)))
    });
    Ok(scored)
}

/// Scales the scores to the best one's, which becomes 1.
///
/// A total that isn't finite and above 0 is an error. Every reason has a positive weight (the
/// seeds, roles, shares and rarities behind it are all positive) and a candidate with none is
/// skipped before it is scored, so no candidate's total is 0 by design, and a NaN or an
/// infinity would scale every score to NaN in a column that can't hold one.
fn scale_to_best(scored: &mut [(Scored, Entry)]) -> Result<(), ScoreError> {
    if let Some((bad, _)) = scored
        .iter()
        .find(|(candidate, _)| !(candidate.score.is_finite() && candidate.score > 0.0))
    {
        return Err(ScoreError::NotAboveZero(bad.candidate, bad.score));
    }
    let best = scored.iter().map(|(s, _)| s.score).fold(0.0, f64::max);
    for (candidate, _) in scored {
        candidate.score /= best;
    }
    Ok(())
}

/// A candidate's place among equal scores, so a sort doesn't depend on a map's order: the
/// masters first, each kind by id.
pub(crate) fn order(candidate: Candidate) -> (bool, u64) {
    let (kind, id) = candidate.kind_id();
    (kind == Kind::Release, id)
}

/// Whether the candidate has artists and every one of them is Various Artists.
pub(crate) fn is_various(entry: &Entry) -> bool {
    !entry.artists.is_empty() && entry.artists.iter().all(|&(id, _)| id == VARIOUS)
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
    let mut kinds: BTreeMap<ReasonKind, Vec<f64>> = BTreeMap::new();
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

/// What a score reads of the seeds.
struct Context<'a> {
    seeds: &'a Seeds,
    /// Each seed's weight, scaled to a mean of 1 over the seeds, 0 out of it.
    weights: Vec<f64>,
    /// Each artist of a seed, with the weights of its seeds summed.
    artist_weights: HashMap<u64, f64>,
    /// The greatest of `artist_weights`.
    max_artist: f64,
    /// Each style's share of the weight of the seeds that have styles.
    shares: HashMap<u32, f64>,
    /// Each style's rarity: the log of all styles' releases over its own.
    rarity: HashMap<u32, f64>,
}

impl<'a> Context<'a> {
    /// The context of `seeds`, or `None` when there are none.
    fn new(seeds: &'a Seeds, connections: &Connections) -> Option<Self> {
        let count = seeds.seeds.len();
        if count == 0 {
            return None;
        }
        let total: f64 = seeds.seeds.iter().map(|seed| seed.weight).sum();
        #[expect(
            clippy::cast_precision_loss,
            reason = "a library's seeds, far below 2^52"
        )]
        let mean = total / count as f64;
        let weights: Vec<f64> = seeds.seeds.iter().map(|seed| seed.weight / mean).collect();
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
        // Only the people the connections keep: a seed's artist left out of them for being on
        // too many releases (Various Artists, a very large artist) would shrink the weight of
        // every other artist and related artist with the compilations in the library.
        let max_artist = artist_weights
            .iter()
            .filter(|(artist, _)| seeds.people.contains_key(artist))
            .map(|(_, &weight)| weight)
            .fold(0.0, f64::max);
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

    /// A candidate's connections to the seeds, from its links and its entry.
    fn reasons(&self, links: &HashSet<Link>, entry: &Entry) -> Result<Vec<Reason>, ScoreError> {
        let mut artists = BTreeSet::new();
        let mut credits: BTreeMap<u64, Vec<u32>> = BTreeMap::new();
        let mut labels: BTreeMap<u64, Vec<u16>> = BTreeMap::new();
        let mut related = BTreeSet::new();
        for link in links {
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
        let mut reasons = Vec::new();
        for person in people {
            reasons.extend(self.person(person, &artists, credits.get(&person))?);
        }
        for (&label, years) in &labels {
            reasons.extend(self.label(label, years)?);
        }
        reasons.extend(self.related(&related)?);
        if !reasons.is_empty()
            && let Some(styles) = self.styles(&entry.styles)
        {
            reasons.push(styles);
        }
        Ok(reasons)
    }

    /// A person on the seeds: by the artist when they are the candidate's artist
    /// and a seed's, a credit otherwise.
    fn person(
        &self,
        person: u64,
        artists: &BTreeSet<u64>,
        roles: Option<&Vec<u32>>,
    ) -> Result<Option<Reason>, ScoreError> {
        let mut parts: Vec<(usize, Role)> = Vec::new();
        let found = find(&self.seeds.people, &person, "person on the seeds")?;
        for &(seed, role) in &found.parts {
            if self.weight(seed)? > 0.0 {
                parts.push((seed, role));
            }
        }
        if parts.is_empty() {
            return Ok(None);
        }
        let albums: BTreeSet<usize> = parts.iter().map(|&(seed, _)| seed).collect();
        let is_artist = artists.contains(&person);
        if is_artist && parts.iter().any(|&(_, role)| role == Role::Artist) {
            let seeds_weight = *find(&self.artist_weights, &person, "weight of the seeds' artist")?;
            return Ok(Some(Reason {
                weight: ARTIST * self.relative(seeds_weight)?,
                connection: Connection::Artist {
                    artist: self.artist(person),
                    albums: parts
                        .iter()
                        .filter(|&&(_, role)| role == Role::Artist)
                        .map(|&(seed, _)| seed)
                        .collect::<BTreeSet<usize>>()
                        .len(),
                },
            }));
        }
        // The candidate's artist counts as a role of weight 1, as the prototype's "artist".
        let (role_weight, role) = if is_artist {
            (1.0, None)
        } else {
            let mut weighted = Vec::new();
            for &role in roles.into_iter().flatten() {
                weighted.push((role, self.role_weight(role)?));
            }
            let (best, best_weight) = weighted
                .into_iter()
                .max_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(&a.0)))
                .ok_or_else(|| {
                    ScoreError::Missing(format!("artist {person} links with no role"))
                })?;
            (best_weight, Some(best))
        };
        let seed_credits: BTreeSet<u32> = parts
            .iter()
            .filter_map(|&(_, role)| match role {
                Role::Artist => None,
                Role::Credit(role) => Some(role),
            })
            .collect();
        let mut seed_roles = Vec::new();
        for role in seed_credits {
            seed_roles.push((role, self.role_weight(role)?));
        }
        seed_roles.sort_by(|a, b| b.1.total_cmp(&a.1));
        let mut seed_weight = 0.0_f64;
        for &(_, role) in &parts {
            seed_weight = seed_weight.max(match role {
                Role::Artist => 1.0,
                Role::Credit(role) => self.role_weight(role)?,
            });
        }
        let shared = self.shared(&albums)?;
        let weight = role_weight * seed_weight * shared.sqrt() * rarity(found.releases) * CREDIT;
        let mut seed_role_names = Vec::new();
        for &(role, _) in &seed_roles {
            seed_role_names.push(self.role_name(role)?);
        }
        let role = role.map(|role| self.role_name(role)).transpose()?;
        Ok(Some(Reason {
            weight,
            connection: Connection::Credit {
                artist: self.artist(person),
                role,
                seed_roles: seed_role_names,
                albums: albums.len(),
            },
        }))
    }

    /// A label of the seeds' families, when the candidate is on it within `YEARS` years of
    /// a seed on the family.
    fn label(&self, label: u64, years: &[u16]) -> Result<Option<Reason>, ScoreError> {
        let mut near = Vec::new();
        let family = find(&self.seeds.families, &label, "family of the label")?;
        for on in &family.on {
            if self.weight(on.seed)? > 0.0
                && years.iter().any(|year| year.abs_diff(on.year) <= YEARS)
            {
                near.push(on);
            }
        }
        if near.is_empty() {
            return Ok(None);
        }
        let albums: BTreeSet<usize> = near.iter().map(|on| on.seed).collect();
        let shared = self.shared(&albums)?;
        let via = if near.iter().any(|on| on.label == label) {
            None
        } else {
            near.iter()
                .map(|on| on.label)
                .min()
                .map(|id| self.label_named(id))
        };
        Ok(Some(Reason {
            weight: shared.sqrt() * rarity(family.releases) * LABEL,
            connection: Connection::Label {
                label: self.label_named(label),
                via,
                albums: albums.len(),
            },
        }))
    }

    /// The aliases, groups and members of the seeds' artists among the
    /// candidate's artists: one reason for each seed artist, by the lowest related id.
    fn related(&self, related: &BTreeSet<u64>) -> Result<Vec<Reason>, ScoreError> {
        let mut by_seed_artist: BTreeMap<u64, (u64, Relation, f64)> = BTreeMap::new();
        for &artist in related {
            for &(relation, seed_artist) in
                find(&self.seeds.related, &artist, "relation of the artist")?
            {
                if let Some(&weight) = self.artist_weights.get(&seed_artist) {
                    by_seed_artist
                        .entry(seed_artist)
                        .or_insert((artist, relation, weight));
                }
            }
        }
        // The weight is relative to the greatest kept seed artist; with none kept, a relation
        // has nothing to weigh against and makes no reason.
        if self.max_artist <= 0.0 {
            return Ok(Vec::new());
        }
        by_seed_artist
            .into_iter()
            .map(|(seed_artist, (artist, relation, weight))| {
                Ok(Reason {
                    weight: RELATED * self.relative(weight)?,
                    connection: Connection::Related {
                        artist: self.artist(artist),
                        relation,
                        of: self.artist(seed_artist),
                    },
                })
            })
            .collect()
    }

    /// The candidate's styles the seeds have, each counted by the root of its
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

    /// An artist's summed seed weight against the greatest of the people kept, from 0 to 1.
    /// A seed's artist that isn't kept, which a related artist can be related to, counts at
    /// most as the greatest. With no kept artist there is no greatest, which an Artist reason
    /// can't reach (its person is kept) and `related` checks for first.
    fn relative(&self, weight: f64) -> Result<f64, ScoreError> {
        if self.max_artist > 0.0 {
            Ok((weight / self.max_artist).min(1.0))
        } else {
            Err(ScoreError::Missing(
                "no kept seed artist to weigh an artist against".to_string(),
            ))
        }
    }

    /// A seed's weight, scaled to a mean of 1.
    fn weight(&self, seed: usize) -> Result<f64, ScoreError> {
        self.weights.get(seed).copied().ok_or_else(|| {
            ScoreError::Missing(format!("no seed {seed}, which a person or label names"))
        })
    }

    /// The summed weights of `seeds`.
    fn shared(&self, seeds: &BTreeSet<usize>) -> Result<f64, ScoreError> {
        let mut shared = 0.0;
        for &seed in seeds {
            shared += self.weight(seed)?;
        }
        Ok(shared)
    }

    fn role_weight(&self, role: u32) -> Result<f64, ScoreError> {
        Ok(find(&self.seeds.roles, &role, "role")?.weight)
    }

    fn role_name(&self, role: u32) -> Result<String, ScoreError> {
        Ok(find(&self.seeds.roles, &role, "role")?.name.clone())
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
    use std::collections::{HashMap, HashSet};

    use catalog::discogs::Entry;

    use super::{
        Connection, Connections, Context, Named, Reason, ScoreError, Scored, VARIOUS, combined,
        is_various, scale_to_best,
    };
    use crate::candidate::Candidate;
    use crate::connection::Link;
    use crate::seed::{Person, Role, Seed, Seeds};

    fn by(artists: &[u64]) -> Entry {
        Entry {
            title: "Title".to_string(),
            year: None,
            artists: artists.iter().map(|&id| (id, None)).collect(),
            styles: Vec::new(),
            main_release: None,
        }
    }

    #[test]
    fn only_an_entry_whose_artists_are_all_various_is_various() {
        assert!(is_various(&by(&[VARIOUS])));
        assert!(!is_various(&by(&[VARIOUS, 7])));
        assert!(!is_various(&by(&[7])));
        // No artists at all is unknown, not Various: the candidate is scored and the
        // evaluation's pool keeps it.
        assert!(!is_various(&by(&[])));
    }

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

    /// A seed by `artist` with weight 1.
    fn seed_by(release_id: u64, artist: u64) -> Seed {
        Seed {
            release_id,
            weight: 1.0,
            artists: vec![artist],
            styles: Vec::new(),
        }
    }

    #[test]
    fn the_greatest_artist_weight_is_taken_over_the_people_kept_only() {
        // Artist 1 is on one seed; artist 99, on three, was left out of `people` for being
        // on too many releases. The weights of 1 and of a related artist are relative to
        // artist 1's, not to the compilations' artist's.
        let seeds = Seeds {
            seeds: vec![
                seed_by(10, 1),
                seed_by(11, 99),
                seed_by(12, 99),
                seed_by(13, 99),
            ],
            people: HashMap::from([(
                1,
                Person {
                    parts: vec![(0, Role::Artist)],
                    releases: 5,
                },
            )]),
            ..Default::default()
        };
        let connections = Connections::default();
        let context = Context::new(&seeds, &connections).unwrap();
        assert!(
            (context.max_artist - context.artist_weights[&1]).abs() < f64::EPSILON,
            "{} against {}",
            context.max_artist,
            context.artist_weights[&1]
        );
        // A related artist of artist 99 weighs at most the related weight in full.
        assert!(
            (context.relative(context.artist_weights[&99]).unwrap() - 1.0).abs() < f64::EPSILON
        );
    }

    #[test]
    fn a_related_artist_has_no_weight_when_no_seed_artist_is_kept() {
        // The only seed artist, 99, is left out of `people` for being on too many releases,
        // so there is no greatest kept artist to weigh the relation against.
        let seeds = Seeds {
            seeds: vec![seed_by(10, 99)],
            related: HashMap::from([(5, vec![(crate::seed::Relation::Alias, 99)])]),
            ..Default::default()
        };
        let connections = Connections::default();
        let context = Context::new(&seeds, &connections).unwrap();
        assert!(context.max_artist.abs() < f64::EPSILON);
        let reasons = context
            .reasons(&HashSet::from([Link::Related(5)]), &by(&[5]))
            .unwrap();
        assert!(reasons.is_empty(), "{reasons:?}");
    }

    /// One seed, by artist 1, who is on it as a credit in role 5; the maps the scoring looks
    /// ids up in hold only what the caller adds.
    fn seeds_of_one_credit() -> Seeds {
        Seeds {
            seeds: vec![seed_by(10, 2)],
            people: HashMap::from([(
                1,
                Person {
                    parts: vec![(0, Role::Credit(5))],
                    releases: 5,
                },
            )]),
            ..Default::default()
        }
    }

    fn missing(problem: &ScoreError) -> &str {
        match problem {
            ScoreError::Missing(problem) => problem,
            ScoreError::NotAboveZero(..) => panic!("{problem:?} is not a missing id"),
        }
    }

    #[test]
    fn a_link_the_seeds_have_no_role_for_is_an_error_not_a_panic() {
        // The seed's credit is in role 5, which has no name row.
        let seeds = seeds_of_one_credit();
        let connections = Connections::default();
        let context = Context::new(&seeds, &connections).unwrap();
        let links = HashSet::from([Link::Credit(1, 5)]);
        let problem = context.reasons(&links, &by(&[3])).unwrap_err();
        assert!(missing(&problem).contains("role"), "{problem:?}");
    }

    #[test]
    fn a_link_to_a_person_a_label_or_an_artist_the_seeds_lack_is_an_error() {
        let seeds = seeds_of_one_credit();
        let connections = Connections::default();
        let context = Context::new(&seeds, &connections).unwrap();
        for (link, id) in [
            (Link::Artist(99), "99"),
            (Link::Credit(99, 5), "99"),
            (Link::Label(77, 2000), "77"),
            (Link::Related(98), "98"),
        ] {
            let problem = context
                .reasons(&HashSet::from([link]), &by(&[3]))
                .unwrap_err();
            assert!(missing(&problem).contains(id), "{link:?}: {problem:?}");
        }
    }

    fn scored(id: u64, score: f64) -> (Scored, Entry) {
        let scored = Scored {
            candidate: Candidate::Release(id),
            score,
            reasons: Vec::new(),
        };
        (scored, by(&[]))
    }

    #[test]
    fn the_scores_are_scaled_to_the_best() {
        let mut list = vec![scored(1, 0.5), scored(3, 2.0)];
        scale_to_best(&mut list).unwrap();
        let ids: Vec<(Candidate, f64)> = list.iter().map(|(s, _)| (s.candidate, s.score)).collect();
        assert_eq!(
            ids,
            [(Candidate::Release(1), 0.25), (Candidate::Release(3), 1.0)]
        );
    }

    #[test]
    fn a_total_that_is_not_above_zero_is_an_error_not_a_dropped_candidate() {
        for total in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let mut list = vec![scored(1, 0.5), scored(2, total)];
            let problem = scale_to_best(&mut list).unwrap_err();
            assert!(
                matches!(problem, ScoreError::NotAboveZero(Candidate::Release(2), _)),
                "{problem:?}"
            );
            assert!(problem.to_string().contains("Release(2)"), "{problem}");
        }
    }
}
