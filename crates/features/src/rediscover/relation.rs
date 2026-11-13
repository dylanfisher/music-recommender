//! Rediscover's relations (ADR 0034): what links a day's album to the albums played in the
//! last `RECENT_DAYS` days, as its reasons, and the day's albums to each other, as threads.
//! Read from the seeds' people, label families and relations between their artists, which
//! `Seeds::read` holds for the profile, with no pass over the catalog. Untagged albums and
//! releases the catalog lacks have no seed, so no links.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use catalog::score::ReasonKind;

use super::{DAY, Id, RECENT_DAYS};
use crate::profile::Album;
use crate::reason::{self, count};
use crate::score::{Connection, Named};
use crate::seed::{Relation, Role, Seeds, YEARS};

/// The most `keys` a link names.
const KEYS: usize = 5;
/// The most threads a day.
const THREADS: usize = 8;
/// The most threads of styles a day: nearly every two albums share a style, so they go
/// after the threads of a person, a relation or a label, and only a few.
const STYLE_THREADS: usize = 2;

/// What links a day's album to albums played in the last `RECENT_DAYS` days.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// `Artist`, `Credit`, `Label` or `Related`.
    pub kind: ReasonKind,
    /// The artist (`Artist`), the person (`Credit`) or the related artist on the albums
    /// played (`Related`).
    pub artist: Option<Named>,
    /// `Credit`: the person's role on this album, `None` when its artist.
    pub role: Option<String>,
    /// `Label`: this album's label.
    pub label: Option<Named>,
    /// `Related`: how `artist` is related to `of`.
    pub relation: Option<Relation>,
    /// `Related`: this album's artist.
    pub of: Option<Named>,
    /// The `key`s of the albums played that share it, the most played first, at most
    /// `KEYS`.
    pub keys: Vec<String>,
    /// How many albums played share it.
    pub albums: usize,
}

impl Link {
    /// The link in words: "Producer: Dee Producer, also on 2 albums you played this month".
    #[must_use]
    pub fn words(&self) -> String {
        let name = |named: &Option<Named>, kind: &str| {
            named
                .as_ref()
                .map_or_else(|| kind.to_string(), |named| reason::name(named, kind))
        };
        let played = format!("{} you played this month", count(self.albums));
        match self.kind {
            ReasonKind::Artist => format!("By {}, like {played}", name(&self.artist, "artist")),
            ReasonKind::Credit => match &self.role {
                Some(role) => format!("{role}: {}, also on {played}", name(&self.artist, "artist")),
                None => format!("By {}, credited on {played}", name(&self.artist, "artist")),
            },
            ReasonKind::Label => format!("On {}, like {played}", name(&self.label, "label")),
            ReasonKind::Related => match (&self.artist, self.relation, &self.of) {
                (Some(artist), Some(relation), Some(of)) => {
                    let related = reason::words(&Connection::Related {
                        artist: artist.clone(),
                        relation,
                        of: of.clone(),
                    });
                    format!("{related}, on {played}")
                }
                _ => format!("Related to {played}"),
            },
            ReasonKind::Styles => format!("Styles of {played}"),
        }
    }

    /// A link of `kind` sharing `artist` or `label`, its albums yet to count.
    fn of(kind: ReasonKind, artist: Option<Named>, label: Option<Named>) -> Self {
        Self {
            kind,
            artist,
            role: None,
            label,
            relation: None,
            of: None,
            keys: Vec::new(),
            albums: 0,
        }
    }

    /// The order links are shown in: the most albums played first, then by kind.
    fn strength(&self) -> (Reverse<usize>, u8) {
        (Reverse(self.albums), order(self.kind))
    }
}

/// A group of the day's albums that share something.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thread {
    /// `Artist`, `Credit`, `Label`, `Related` or `Styles`.
    pub kind: ReasonKind,
    /// The thread in words, for showing.
    pub words: String,
    /// Its albums' ids, in the day's order.
    pub ids: Vec<Id>,
    /// The person or artist it shares (`Artist`, `Credit`, `Related`).
    pub artist: Option<Named>,
    /// The label it shares (`Label`).
    pub label: Option<Named>,
}

/// The kinds in the order a tie between two links or threads goes by.
fn order(kind: ReasonKind) -> u8 {
    match kind {
        ReasonKind::Artist => 0,
        ReasonKind::Credit => 1,
        ReasonKind::Related => 2,
        ReasonKind::Label => 3,
        ReasonKind::Styles => 4,
    }
}

/// The seeds' links, indexed by seed, and the albums played recently.
pub(crate) struct Relations<'a> {
    seeds: &'a Seeds,
    /// Each release's seed.
    seed_of: HashMap<u64, usize>,
    /// Each seed's people and their parts on it, credits in a weak role left out.
    parts: Vec<Vec<(u64, Role)>>,
    /// Each seed's label families: the family's label, the seed's own label in it, and the
    /// seed's year.
    families: Vec<Vec<(u64, u64, u16)>>,
    /// The albums played in the last `RECENT_DAYS` days, by seed, as their key and plays.
    recent: HashMap<usize, Vec<(String, u64)>>,
}

impl<'a> Relations<'a> {
    /// The links of `seeds`, read for `albums`, with the albums played within
    /// `RECENT_DAYS` days of `now`.
    pub(crate) fn new(seeds: &'a Seeds, albums: &[Album], now: i64) -> Self {
        let seed_of: HashMap<u64, usize> = seeds
            .seeds
            .iter()
            .enumerate()
            .map(|(index, seed)| (seed.release_id, index))
            .collect();
        let mut parts = vec![Vec::new(); seeds.seeds.len()];
        for (&person, found) in &seeds.people {
            for &(seed, role) in &found.parts {
                if !is_weak(seeds, role) {
                    parts[seed].push((person, role));
                }
            }
        }
        for seed_parts in &mut parts {
            seed_parts.sort_unstable_by_key(|&(person, role)| (person, role_order(role)));
        }
        let mut families = vec![Vec::new(); seeds.seeds.len()];
        for (&family, found) in &seeds.families {
            for on in &found.on {
                families[on.seed].push((family, on.label, on.year));
            }
        }
        for seed_families in &mut families {
            seed_families.sort_unstable();
        }
        let mut recent: HashMap<usize, Vec<(String, u64)>> = HashMap::new();
        for album in albums {
            let played_recently = album
                .last_played
                .is_some_and(|last| now.saturating_sub(last) < RECENT_DAYS * DAY);
            let seed = album.discogs_release_id.and_then(|id| seed_of.get(&id));
            if let (true, Some(&seed)) = (played_recently, seed) {
                recent
                    .entry(seed)
                    .or_default()
                    .push((album.key.clone(), album.plays));
            }
        }
        Self {
            seeds,
            seed_of,
            parts,
            families,
            recent,
        }
    }

    /// The links of the album tagged with `release_id` to the albums played recently,
    /// strongest first. None for an untagged album or a release with no seed.
    pub(crate) fn links(&self, release_id: Option<u64>) -> Vec<Link> {
        let Some(&seed) = release_id.and_then(|id| self.seed_of.get(&id)) else {
            return Vec::new();
        };
        let is_recent = |other: usize| other != seed && self.recent.contains_key(&other);
        // Each link, by what it shares, with the recent seeds that share it.
        let mut shared: BTreeMap<(u8, u64), (Link, BTreeSet<usize>)> = BTreeMap::new();
        let mut add = |key: (u8, u64), link: Link, other: usize| {
            shared
                .entry(key)
                .or_insert_with(|| (link, BTreeSet::new()))
                .1
                .insert(other);
        };
        for &(person, role) in &self.parts[seed] {
            for &(other, other_role) in &self.seeds.people[&person].parts {
                if !is_recent(other) || is_weak(self.seeds, other_role) {
                    continue;
                }
                let kind = if role == Role::Artist && other_role == Role::Artist {
                    ReasonKind::Artist
                } else {
                    ReasonKind::Credit
                };
                let role = match role {
                    Role::Credit(role) if kind == ReasonKind::Credit => {
                        self.seeds.roles.get(&role).map(|role| role.name.clone())
                    }
                    _ => None,
                };
                let link = Link {
                    role,
                    ..Link::of(kind, Some(self.artist(person)), None)
                };
                add((order(kind), person), link, other);
            }
        }
        for &(family, label, year) in &self.families[seed] {
            for on in &self.seeds.families[&family].on {
                if is_recent(on.seed) && on.year.abs_diff(year) <= YEARS {
                    let link = Link::of(ReasonKind::Label, None, Some(self.label(label)));
                    add((order(ReasonKind::Label), label), link, on.seed);
                }
            }
        }
        let artists = &self.seeds.seeds[seed].artists;
        let mut others: Vec<usize> = self.recent.keys().copied().collect();
        others.sort_unstable();
        for other in others {
            if !is_recent(other) {
                continue;
            }
            for &played in &self.seeds.seeds[other].artists {
                let ways = self.seeds.between.get(&played).into_iter().flatten();
                for &(relation, of) in ways.filter(|(_, of)| artists.contains(of)) {
                    let link = Link {
                        relation: Some(relation),
                        of: Some(self.artist(of)),
                        ..Link::of(ReasonKind::Related, Some(self.artist(played)), None)
                    };
                    add((order(ReasonKind::Related), played), link, other);
                }
            }
        }
        let mut links: Vec<Link> = shared
            .into_values()
            .map(|(link, others)| {
                let mut played: Vec<&(String, u64)> = others
                    .iter()
                    .flat_map(|other| &self.recent[other])
                    .collect();
                played.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                Link {
                    albums: played.len(),
                    keys: played
                        .iter()
                        .take(KEYS)
                        .map(|(key, _)| key.clone())
                        .collect(),
                    ..link
                }
            })
            .collect();
        // Stable, so a tie keeps the order of what it shares.
        links.sort_by_key(Link::strength);
        links
    }

    /// The threads among the day's albums, given in the day's order as their id and
    /// release, strongest first, at most `THREADS`: what two or more of them share.
    pub(crate) fn threads(&self, day: &[(Id, Option<u64>)]) -> Vec<Thread> {
        let seeds: Vec<(Id, usize)> = day
            .iter()
            .filter_map(|&(id, release_id)| Some((id, *self.seed_of.get(&release_id?)?)))
            .collect();
        // Each thread by what it shares, with the positions of its albums in `seeds`, and
        // whether every one is there as an artist (a person's) or one through a relation.
        let mut people: BTreeMap<u64, (BTreeSet<usize>, bool)> = BTreeMap::new();
        let mut labels: BTreeMap<u64, BTreeMap<usize, u16>> = BTreeMap::new();
        let mut related: BTreeMap<u64, (BTreeSet<usize>, bool)> = BTreeMap::new();
        let mut styles: BTreeMap<u32, BTreeSet<usize>> = BTreeMap::new();
        for (position, &(_, seed)) in seeds.iter().enumerate() {
            for &(person, role) in &self.parts[seed] {
                let entry = people.entry(person).or_insert((BTreeSet::new(), true));
                entry.0.insert(position);
                entry.1 &= role == Role::Artist;
            }
            for &(family, label, year) in &self.families[seed] {
                if family == label {
                    labels.entry(label).or_default().insert(position, year);
                }
            }
            for &artist in &self.seeds.seeds[seed].artists {
                related.entry(artist).or_default().0.insert(position);
                for &(_, of) in self.seeds.between.get(&artist).into_iter().flatten() {
                    let entry = related.entry(of).or_default();
                    entry.0.insert(position);
                    entry.1 = true;
                }
            }
            for &style in &self.seeds.seeds[seed].styles {
                styles.entry(style).or_default().insert(position);
            }
        }
        let ids = |positions: &mut dyn Iterator<Item = usize>| -> Vec<Id> {
            positions.map(|position| seeds[position].0).collect()
        };
        let mut threads: Vec<Thread> = Vec::new();
        for (person, (positions, as_artist)) in people {
            let (kind, words) = if as_artist {
                (ReasonKind::Artist, "By")
            } else {
                (ReasonKind::Credit, "With")
            };
            let artist = self.artist(person);
            threads.push(Thread {
                kind,
                words: format!("{words} {}", reason::name(&artist, "artist")),
                ids: ids(&mut positions.into_iter()),
                artist: Some(artist),
                label: None,
            });
        }
        for (artist, (positions, through_relation)) in related {
            if through_relation {
                let artist = self.artist(artist);
                threads.push(Thread {
                    kind: ReasonKind::Related,
                    words: format!("{} and related artists", reason::name(&artist, "artist")),
                    ids: ids(&mut positions.into_iter()),
                    artist: Some(artist),
                    label: None,
                });
            }
        }
        for (label, years) in labels {
            let label = self.label(label);
            let (first, last) = (years.values().min(), years.values().max());
            let span = match (first, last) {
                (Some(first), Some(last)) if first != last => format!(", {first} to {last}"),
                (Some(year), _) => format!(", {year}"),
                _ => String::new(),
            };
            threads.push(Thread {
                kind: ReasonKind::Label,
                words: format!("{}{span}", reason::name(&label, "label")),
                ids: ids(&mut years.into_keys()),
                artist: None,
                label: Some(label),
            });
        }
        for (style, positions) in styles {
            let name = self.seeds.style_names.get(&style).cloned();
            threads.push(Thread {
                kind: ReasonKind::Styles,
                words: reason::name_or_id(u64::from(style), name.as_deref(), "style"),
                ids: ids(&mut positions.into_iter()),
                artist: None,
                label: None,
            });
        }
        strongest(threads)
    }

    fn artist(&self, id: u64) -> Named {
        Named::new(id, self.seeds.artist_names.get(&id).cloned())
    }

    fn label(&self, id: u64) -> Named {
        Named::new(id, self.seeds.label_names.get(&id).cloned())
    }
}

/// The threads of two albums or more of `threads`, strongest first, at most `THREADS`:
/// those of a person, a relation or a label by their albums, the most first, then those of a
/// style, at most `STYLE_THREADS`. A thread of the same albums as a stronger one is left out.
fn strongest(mut threads: Vec<Thread>) -> Vec<Thread> {
    threads.retain(|thread| thread.ids.len() >= 2);
    // Stable, so a tie keeps the order of the id it shares.
    threads.sort_by_key(|thread| {
        (
            thread.kind == ReasonKind::Styles,
            Reverse(thread.ids.len()),
            order(thread.kind),
        )
    });
    let mut seen: BTreeSet<Vec<Id>> = BTreeSet::new();
    let mut style_threads = 0;
    threads.retain(|thread| {
        if !seen.insert(thread.ids.clone()) {
            return false;
        }
        if thread.kind == ReasonKind::Styles {
            style_threads += 1;
        }
        style_threads <= STYLE_THREADS
    });
    threads.truncate(THREADS);
    threads
}

/// Whether `role` is a credit in a role that says little about the music.
fn is_weak(seeds: &Seeds, role: Role) -> bool {
    match role {
        Role::Artist => false,
        Role::Credit(role) => seeds.roles.get(&role).is_none_or(|role| role.weight < 1.0),
    }
}

/// An artist's part before a credit's, then credits by role id.
fn role_order(role: Role) -> (bool, u32) {
    match role {
        Role::Artist => (false, 0),
        Role::Credit(role) => (true, role),
    }
}

#[cfg(test)]
mod tests {
    use catalog::score::ReasonKind;

    use super::{THREADS, Thread, strongest};
    use crate::rediscover::Id;

    fn thread(kind: ReasonKind, ids: &[u64]) -> Thread {
        Thread {
            kind,
            words: String::new(),
            ids: ids.iter().map(|&id| Id::Release(id)).collect(),
            artist: None,
            label: None,
        }
    }

    #[test]
    fn styles_come_last_and_few_and_a_repeat_or_a_lone_album_is_no_thread() {
        let mut threads = vec![
            thread(ReasonKind::Styles, &[1, 2, 3, 4, 5]),
            thread(ReasonKind::Styles, &[1, 2, 3, 4]),
            thread(ReasonKind::Styles, &[1, 2, 3]),
            thread(ReasonKind::Label, &[1]),
            thread(ReasonKind::Label, &[1, 2]),
            // The same albums as a stronger kind's.
            thread(ReasonKind::Label, &[3, 4]),
            thread(ReasonKind::Credit, &[3, 4]),
            thread(ReasonKind::Artist, &[5, 6, 7]),
        ];
        let kinds = |threads: &[Thread]| -> Vec<(ReasonKind, usize)> {
            threads.iter().map(|t| (t.kind, t.ids.len())).collect()
        };
        assert_eq!(
            kinds(&strongest(threads.clone())),
            [
                (ReasonKind::Artist, 3),
                (ReasonKind::Credit, 2),
                (ReasonKind::Label, 2),
                (ReasonKind::Styles, 5),
                (ReasonKind::Styles, 4),
            ]
        );
        threads.extend((10..20).map(|id| thread(ReasonKind::Label, &[id, id + 100])));
        let kept = strongest(threads);
        assert_eq!(kept.len(), THREADS);
        assert!(kept.iter().all(|t| t.kind != ReasonKind::Styles));
    }
}
