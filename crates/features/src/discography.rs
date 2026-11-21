//! An artist's discography (ADR 0037): every album the catalog has the artist as a main
//! artist of, whatever its format, owned and marked albums included, with no score, pool or
//! cap, in the order asked. Its masters are found through the artist's releases, kept when
//! the master names the artist among its own artists, and its releases without a master
//! are its own albums. The values a page is missing, or has from over 90 days ago, are queued
//! in the sync's discography tier when it is asked for. `POST /v1/recommendations` answers
//! `list: "discography"` through [`discography`].

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use catalog::community::{Community, REFRESH_AFTER, Stored};
use catalog::discogs::Catalog;
use catalog::error::DatabaseError;
use catalog::score::Filter;
use config::Config;

use crate::candidate::Candidate;
use crate::clock::{Clock, unix_seconds};
use crate::community::ReadError;
use crate::detail::{self, Detail, Details};
use crate::list::{self, ListError, Page};
use crate::rescore;
use crate::score::Named;
use crate::sort::{self, Sort};

/// How many discographies the sync's discography tier keeps, the last asked; an earlier
/// one's requests not done move to the asked lists' tier.
const DISCOGRAPHIES: usize = 5;

/// The most releases one request for a discography queues: the first in its order that need
/// values.
const QUEUED: usize = 500;

/// The order a discography is shown in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Order {
    /// The oldest first, albums without a year last.
    Year,
    /// By title, ignoring case.
    Title,
    /// By a community value, the highest first, albums without values last. Never
    /// `Sort::Score`: a discography has no score.
    Value(Sort),
}

impl Order {
    /// The order named `name`, as a request names it; `None` for a name not listed and for
    /// `score`.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        match name {
            "year" => Some(Self::Year),
            "title" => Some(Self::Title),
            name => Sort::named(name)
                .filter(|sort| *sort != Sort::Score)
                .map(Self::Value),
        }
    }
}

/// A discography asked for.
#[derive(Debug, Clone)]
pub struct Ask {
    /// The Discogs artist.
    pub artist_id: u64,
    /// The albums to keep.
    pub filter: Filter,
    /// Their order.
    pub order: Order,
    /// How many of those kept to skip.
    pub offset: usize,
    /// How many to send.
    pub limit: usize,
}

/// An album of a discography.
#[derive(Debug, Clone)]
pub struct Album {
    /// Its place in the whole discography in the order asked, from 1, before the filter.
    pub rank: u64,
    /// The master, or the release without one.
    pub candidate: Candidate,
    /// The release whose community values stand for it, as everywhere else
    /// ([`Candidate::representative`]); `None` for a master the catalog has no row for.
    pub representative: Option<u64>,
    /// Its title.
    pub title: String,
    /// Its year, a master's own.
    pub year: Option<u16>,
    /// Its artists in order.
    pub artists: Vec<Named>,
    /// Its representative's first label, `None` with none or a "Not On Label".
    pub label: Option<Named>,
    /// Its styles.
    pub styles: Vec<Named>,
    /// Its formats, whether the library has it, and its verdict.
    pub detail: Detail,
}

/// A page of a discography.
#[derive(Debug)]
pub struct Discography {
    /// The dumps' set (`YYYYMMDD`) the catalog was built from.
    pub catalog_set: String,
    /// The artist, with its name.
    pub artist: Named,
    /// How many albums the whole discography has, before the filter.
    pub albums: u64,
    /// The page.
    pub page: Page<Album>,
    /// How many of its albums' releases the sync is still to fetch values for, at most
    /// `QUEUED`: queued this time, or before and not fetched since.
    pub pending: u64,
}

/// The page `ask` asks for of an artist's discography, from the catalog, the kept profile,
/// the community values and the feedback as they are now, at `clock`'s time. The releases
/// of the whole discography without values, or with values due for a refresh, are queued in
/// the sync's discography tier, the first `QUEUED` in the order asked.
///
/// # Errors
///
/// When there is no catalog or kept profile, the catalog has no such artist, or a database
/// can't be read or written.
pub fn discography(
    config: &Config,
    clock: &dyn Clock,
    ask: &Ask,
) -> Result<Discography, ListError> {
    let data_dir = &config.data_dir;
    let (catalog, profile) = list::load(data_dir)?;
    let name = catalog
        .artist_name(ask.artist_id)?
        .ok_or(ListError::UnknownArtist(ask.artist_id))?;
    let mut db = Community::open(data_dir).map_err(community)?;
    let albums = albums_of(&catalog, &db, ask.artist_id)?;
    let ids: Vec<u64> = albums
        .iter()
        .filter_map(|album| album.representative)
        .collect();
    let stored: HashMap<u64, Stored> = ids
        .iter()
        .copied()
        .zip(db.stored(&ids).map_err(community)?)
        .collect();
    let stored_of = |album: &Album| album.representative.and_then(|id| stored.get(&id));
    let mean = match ask.order {
        Order::Value(Sort::Rating) => db.rating_mean().map_err(community)?,
        _ => None,
    };
    let keys: Vec<Option<f64>> = albums
        .iter()
        .map(|album| match ask.order {
            Order::Value(by) => stored_of(album)
                .and_then(sort::values)
                .map(|values| sort::key(&values, by, mean)),
            Order::Year | Order::Title => None,
        })
        .collect();
    let mut order: Vec<usize> = (0..albums.len()).collect();
    order.sort_by(|&a, &b| compare(&albums[a], &albums[b], keys[a], keys[b], ask.order));
    let mut albums: Vec<Album> = {
        let mut places: Vec<Option<Album>> = albums.into_iter().map(Some).collect();
        order
            .iter()
            .filter_map(|&place| places[place].take())
            .collect()
    };
    for (album, rank) in albums.iter_mut().zip(1..) {
        album.rank = rank;
    }
    let now = clock.now();
    let wanted: Vec<u64> = albums
        .iter()
        .filter_map(|album| {
            let id = album.representative?;
            needs_values(stored.get(&id), unix_seconds(now)).then_some(id)
        })
        .take(QUEUED)
        .collect();
    if !wanted.is_empty() {
        // To the nanosecond, so two discographies asked in one second stay two in the tier.
        let at = match now.duration_since(std::time::UNIX_EPOCH) {
            Ok(after) => after.as_secs_f64(),
            Err(before) => -before.duration().as_secs_f64(),
        };
        db.queue_discography(&wanted, at, unix_seconds(now), DISCOGRAPHIES)
            .map_err(community)?;
    }
    let total = albums.len() as u64;
    let kept: Vec<Album> = albums
        .into_iter()
        .filter(|album| keeps(&ask.filter, album))
        .collect();
    let more = ask.offset.saturating_add(ask.limit) < kept.len();
    let mut items: Vec<Album> = kept.into_iter().skip(ask.offset).take(ask.limit).collect();
    let library = detail::library(&profile);
    let details = Details::read(&catalog, &library, data_dir)?;
    for album in &mut items {
        album.detail = details.of(album.candidate, album.representative)?;
    }
    let catalog_set = catalog
        .summary()?
        .imported
        .first()
        .map(|set| set.date.clone())
        .ok_or_else(|| catalog.inconsistent("no dump set recorded"))?;
    Ok(Discography {
        catalog_set,
        artist: Named::new(ask.artist_id, Some(name)),
        albums: total,
        page: Page { items, more },
        pending: wanted.len() as u64,
    })
}

/// The albums `artist_id` is a main artist of in `catalog`, each once, in no order, with
/// their representatives as `db`'s versions choose them and no detail yet.
fn albums_of(catalog: &Catalog, db: &Community, artist_id: u64) -> Result<Vec<Album>, ListError> {
    let mut seen = HashSet::new();
    let candidates: Vec<Candidate> = catalog
        .main_releases(artist_id)?
        .into_iter()
        .map(|(release_id, master_id)| Candidate::of(release_id, master_id))
        .filter(|candidate| seen.insert(*candidate))
        .collect();
    let style_names = catalog.style_names()?;
    let mut labels = HashMap::new();
    let mut albums = Vec::new();
    for candidate in candidates {
        let entry = candidate.entry(catalog)?.ok_or_else(|| {
            catalog.inconsistent(format!("{candidate:?} of a release has no entry"))
        })?;
        // A master is the artist's when it names the artist among its own artists, not
        // when only some release of it does.
        if !entry.artists.iter().any(|&(id, _)| id == artist_id) {
            continue;
        }
        let most_owned = match candidate {
            Candidate::Master(id) => db.most_owned(id).map_err(community)?,
            Candidate::Release(_) => None,
        };
        let representative = candidate.representative(entry.main_release, most_owned);
        let label = match representative {
            Some(id) => match catalog.first_label(id)? {
                Some(first) => rescore::label(catalog, &mut labels, first)?,
                None => None,
            },
            None => None,
        };
        albums.push(Album {
            rank: 0,
            candidate,
            representative,
            title: entry.title,
            year: entry.year,
            artists: entry
                .artists
                .into_iter()
                .map(|(id, name)| Named::new(id, name))
                .collect(),
            label,
            styles: entry
                .styles
                .iter()
                .map(|&id| Named::new(u64::from(id), style_names.get(&id).cloned()))
                .collect(),
            detail: Detail {
                formats: Vec::new(),
                in_library: false,
                mark: None,
            },
        });
    }
    Ok(albums)
}

/// The community database's failure, as a list's.
fn community(error: DatabaseError) -> ListError {
    ListError::Community(ReadError::Database(error))
}

/// Whether the release `stored` is of has no values the sync will fetch on its own soon, so
/// a discography asks for them: never fetched, queued and not fetched yet, or fetched or
/// given up on over `REFRESH_AFTER` before `now`. A release Discogs answered 404 for has none
/// to fetch.
fn needs_values(stored: Option<&Stored>, now: i64) -> bool {
    let due = |at: i64| at <= now - REFRESH_AFTER.as_secs().cast_signed();
    match stored {
        None | Some(Stored::NotQueued | Stored::Queued) => true,
        Some(Stored::Fetched { at, .. } | Stored::Failed { at, .. }) => due(*at),
        Some(Stored::Gone { .. }) => false,
    }
}

/// Whether `filter` keeps `album`, as it keeps a scored list's: any of its styles by name,
/// ignoring ASCII case; its year in the range; its label among the ids.
fn keeps(filter: &Filter, album: &Album) -> bool {
    let styles = filter.styles.is_empty()
        || album.styles.iter().any(|style| {
            style.name.as_ref().is_some_and(|name| {
                filter
                    .styles
                    .iter()
                    .any(|wanted| wanted.eq_ignore_ascii_case(name))
            })
        });
    let years = filter.years.is_none_or(|(first, last)| {
        album
            .year
            .is_some_and(|year| (first..=last).contains(&year))
    });
    let labels = filter.label_ids.is_empty()
        || album
            .label
            .as_ref()
            .is_some_and(|label| filter.label_ids.contains(&label.id));
    styles && years && labels
}

/// The order of `a` and `b`, whose values for `order` are `a_key` and `b_key`: by the
/// order asked, then by year, then title, then kind and id, so the order is total and the
/// same each time.
fn compare(a: &Album, b: &Album, a_key: Option<f64>, b_key: Option<f64>, order: Order) -> Ordering {
    let year = || match (a.year, b.year) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    };
    let title = || {
        a.title
            .to_lowercase()
            .cmp(&b.title.to_lowercase())
            .then_with(|| a.title.cmp(&b.title))
    };
    let id = || a.candidate.kind_id().cmp(&b.candidate.kind_id());
    let first = match order {
        Order::Year | Order::Title => Ordering::Equal,
        // The highest first, and every album without values after those with.
        Order::Value(_) => match (a_key, b_key) {
            (Some(a), Some(b)) => b.total_cmp(&a),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        },
    };
    let by = if order == Order::Title {
        first.then_with(title).then_with(year)
    } else {
        first.then_with(year).then_with(title)
    };
    by.then_with(id)
}
