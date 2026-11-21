//! `POST /v1/recommendations` (docs/api.md, Recommendations): a page of a list's albums or
//! artists, the whole library's or a seed album's or artist's, filtered, read strictly and
//! answered in the contract's shape through `features::list`, which `recommend` prints
//! through too (ADR 0028).

use std::collections::HashMap;

use config::Config;
use features::candidate::Candidate;
use features::clock::{Clock, unix_seconds};
use features::community::{self, Stored};
use features::detail::Detail;
use features::discography::{self, Ask, Order};
use features::feedback::Verdict;
use features::list::{self, Filter, List, ListError, Of, SeedSlots};
use features::reason;
use features::rescore::Listed;
use features::score::{Connection, Named};
use features::sort::Sort;
use serde_json::{Map, Value, json};

use crate::body::{BODY, array, bad, integer, internal, only, required, string, strings};
use crate::community::release;
use crate::reply::{Refusal, Reply};

/// The fields every request may have, whatever it shows.
const FIELDS: [&str; 7] = [
    "list",
    "release_id",
    "artist",
    "artist_id",
    "show",
    "offset",
    "limit",
];

/// The list of an artist's albums by the catalog, which has no score (ADR 0037).
const DISCOGRAPHY: &str = "discography";

/// The fields that filter albums, which the artists' list refuses. A request may have these
/// and `FIELDS` and no others.
const FILTERS: [&str; 5] = ["missing", "styles", "year_from", "year_to", "label_ids"];

/// The field that orders albums, which the artists' list refuses too.
const SORT: &str = "sort";

/// The most albums or artists in a reply; `list::LIMIT` unless asked. An album is about
/// 2 KB with three reasons, long names and its community values, and an artist about as
/// much with its best album, so 100 stay near 200 KB, well under the player's 1 MB cap.
const MAX_LIMIT: usize = 100;

/// The most style names, and label ids, a filter holds.
const MAX_STYLES: usize = 50;
const MAX_LABELS: usize = 100;

/// What a request asks for, read from its body.
struct Asked {
    of: Of,
    show: Show,
    offset: usize,
    limit: usize,
}

/// Which of a list's two a request pages through.
enum Show {
    /// Its albums, with the filter, whether only the missing ones, and their order.
    Albums(Filter, bool, Sort),
    /// Its artists.
    Artists,
}

/// Answers with a page of the list asked for. A seed list kept on disk is read at once; one
/// that isn't is computed first, which the reply waits for, as long as a pass over the
/// catalog takes.
pub(crate) fn answer(
    body: &Map<String, Value>,
    config: &Config,
    clock: &dyn Clock,
    slots: &SeedSlots,
) -> Result<Reply, Refusal> {
    only(
        body,
        BODY,
        &[FIELDS.as_slice(), FILTERS.as_slice(), &[SORT]].concat(),
    )?;
    if string(required(body, "list")?, "list")? == DISCOGRAPHY {
        let ask = discography_asked(body)?;
        return match discography::discography(config, clock, &ask) {
            Ok(found) => discography_page(&found, config, clock),
            Err(error) => refused(&error),
        };
    }
    let asked = asked(body)?;
    match List::open_within(
        config,
        clock,
        &asked.of,
        &mut |line| println!("{line}"),
        Some(slots),
    ) {
        Ok(list) => page(&list, &asked, config),
        Err(error) => refused(&error),
    }
}

/// The request for a scored list in `body`, every field checked.
fn asked(body: &Map<String, Value>) -> Result<Asked, Refusal> {
    let of = match string(required(body, "list")?, "list")?.as_str() {
        "library" => Of::Library,
        "album" => {
            Of::Album(integer(required(body, "release_id")?, "release_id", 1)?.cast_unsigned())
        }
        "artist" => Of::Artist(string(required(body, "artist")?, "artist")?),
        other => return Err(bad(&format!("list can't be {other:?}"))),
    };
    if body.contains_key("release_id") && !matches!(of, Of::Album(_)) {
        return Err(bad("release_id is for the album list only"));
    }
    if body.contains_key("artist") && !matches!(of, Of::Artist(_)) {
        return Err(bad("artist is for the artist list only"));
    }
    if body.contains_key("artist_id") {
        return Err(bad("artist_id is for the discography list only"));
    }
    let show = match string(required(body, "show")?, "show")?.as_str() {
        "albums" => Show::Albums(filter(body)?, missing(body)?, sort(body)?),
        "artists" => {
            if body.contains_key(SORT) {
                return Err(bad("sort orders albums; the artists' list takes no sort"));
            }
            if let Some(field) = FILTERS.iter().find(|field| body.contains_key(**field)) {
                return Err(bad(&format!(
                    "{field} filters albums; the artists' list takes no filter"
                )));
            }
            Show::Artists
        }
        other => return Err(bad(&format!("show can't be {other:?}"))),
    };
    let (offset, limit) = paged(body)?;
    Ok(Asked {
        of,
        show,
        offset,
        limit,
    })
}

/// The request for a discography in `body`, every field checked: `artist_id`, albums only,
/// none of the scored lists' seeds or `missing`, and a sort other than `score`.
fn discography_asked(body: &Map<String, Value>) -> Result<Ask, Refusal> {
    let artist_id = integer(required(body, "artist_id")?, "artist_id", 1)?.cast_unsigned();
    if let Some(field) = ["release_id", "artist"]
        .iter()
        .find(|field| body.contains_key(**field))
    {
        return Err(bad(&format!(
            "{field} is for a scored list; a discography takes artist_id"
        )));
    }
    if body.contains_key("missing") {
        return Err(bad(
            "missing is for a scored list; a discography shows every album",
        ));
    }
    match string(required(body, "show")?, "show")?.as_str() {
        "albums" => {}
        other => {
            return Err(bad(&format!(
                "show can't be {other:?}; a discography shows albums"
            )));
        }
    }
    let order = match body.get(SORT) {
        None => Order::Year,
        Some(value) => {
            let name = string(value, SORT)?;
            Order::named(&name).ok_or_else(|| {
                bad(&format!(
                    "sort can't be {name:?} for a discography: year, title, rating, have, want \
                     or want_have"
                ))
            })?
        }
    };
    let (offset, limit) = paged(body)?;
    Ok(Ask {
        artist_id,
        filter: filter(body)?,
        order,
        offset,
        limit,
    })
}

/// The page's `offset` and `limit` in `body`, 0 and `list::LIMIT` unless sent.
fn paged(body: &Map<String, Value>) -> Result<(usize, usize), Refusal> {
    let offset = match body.get("offset") {
        None => 0,
        Some(value) => {
            usize::try_from(integer(value, "offset", 0)?).map_err(|_| bad("offset is too large"))?
        }
    };
    let limit = match body.get("limit") {
        None => list::LIMIT,
        Some(value) => {
            let limit = usize::try_from(integer(value, "limit", 1)?)
                .ok()
                .filter(|limit| *limit <= MAX_LIMIT);
            limit.ok_or_else(|| bad(&format!("limit must be from 1 to {MAX_LIMIT}")))?
        }
    };
    Ok((offset, limit))
}

/// The albums' order in `body`, by score unless sent.
fn sort(body: &Map<String, Value>) -> Result<Sort, Refusal> {
    match body.get(SORT) {
        None => Ok(Sort::Score),
        Some(value) => {
            let name = string(value, SORT)?;
            Sort::named(&name).ok_or_else(|| {
                bad(&format!(
                    "sort can't be {name:?} for a scored list: score, rating, have, want or \
                     want_have"
                ))
            })
        }
    }
}

fn missing(body: &Map<String, Value>) -> Result<bool, Refusal> {
    match body.get("missing") {
        None => Ok(false),
        Some(Value::Bool(missing)) => Ok(*missing),
        Some(_) => Err(bad("missing must be true or false")),
    }
}

/// The albums' filter in `body`; a field left out filters nothing.
fn filter(body: &Map<String, Value>) -> Result<Filter, Refusal> {
    let styles = body
        .get("styles")
        .map(|value| strings(value, "styles", MAX_STYLES))
        .transpose()?
        .unwrap_or_default();
    let year = |field: &str| {
        body.get(field)
            .map(|value| {
                u16::try_from(integer(value, field, 1)?)
                    .map_err(|_| bad(&format!("{field} must be a year from 1 to {}", u16::MAX)))
            })
            .transpose()
    };
    let (from, to) = (year("year_from")?, year("year_to")?);
    if let (Some(from), Some(to)) = (from, to)
        && from > to
    {
        return Err(bad("year_from is after year_to"));
    }
    let years =
        (from.is_some() || to.is_some()).then(|| (from.unwrap_or(0), to.unwrap_or(u16::MAX)));
    let label_ids = match body.get("label_ids") {
        None => Vec::new(),
        Some(value) => {
            let items = array(value, "label_ids")?;
            if items.len() > MAX_LABELS {
                return Err(bad(&format!("label_ids holds more than {MAX_LABELS}")));
            }
            items
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    integer(item, &format!("label_ids[{index}]"), 1).map(i64::cast_unsigned)
                })
                .collect::<Result<_, _>>()?
        }
    };
    Ok(Filter {
        styles,
        years,
        label_ids,
    })
}

/// The page `asked` asks for of `list`, with each album's community values.
fn page(list: &List, asked: &Asked, config: &Config) -> Result<Reply, Refusal> {
    let computed = list.computed();
    let mut reply = Map::new();
    reply.insert(
        "list".to_string(),
        json!({
            "catalog_set": computed.inputs.catalog_set,
            "computed_at": computed.finished_at,
            "seeds": computed.seeds,
            "candidates": computed.candidates,
            "artist": null,
        }),
    );
    let (albums, artists, more) = match &asked.show {
        Show::Albums(filter, missing, sort) => {
            let page = list
                .albums(filter, *missing, *sort, asked.offset, asked.limit)
                .map_err(internal)?;
            let community = values(config, page.items.iter().map(|album| album.representative))?;
            let details = list.details(&page.items).map_err(internal)?;
            let albums: Vec<Value> = page
                .items
                .iter()
                .zip(&details)
                .map(|(album, detail)| album_value(album, &community, detail))
                .collect();
            (Value::Array(albums), Value::Null, page.more)
        }
        Show::Artists => {
            let page = list.artists(asked.offset, asked.limit).map_err(internal)?;
            let community = values(
                config,
                page.items.iter().map(|artist| artist.best.representative),
            )?;
            let details = list
                .details(page.items.iter().map(|artist| &artist.best))
                .map_err(internal)?;
            let artists: Vec<Value> = page
                .items
                .iter()
                .zip(&details)
                .map(|(artist, detail)| {
                    json!({
                        "rank": artist.rank,
                        "id": artist.id,
                        "name": artist.name,
                        "score": artist.score,
                        "albums": artist.albums,
                        "best": album_value(&artist.best, &community, detail),
                    })
                })
                .collect();
            (Value::Null, Value::Array(artists), page.more)
        }
    };
    reply.insert("albums".to_string(), albums);
    reply.insert("artists".to_string(), artists);
    reply.insert("more".to_string(), json!(more));
    reply.insert("pending".to_string(), Value::Null);
    Ok(Reply::ok(reply))
}

/// A page of a discography in the contract's shape, the scored lists' with no artists, its
/// artist in `list`, and `pending`.
fn discography_page(
    found: &discography::Discography,
    config: &Config,
    clock: &dyn Clock,
) -> Result<Reply, Refusal> {
    let items = &found.page.items;
    let community = values(config, items.iter().map(|album| album.representative))?;
    let albums: Vec<Value> = items
        .iter()
        .map(|album| discography_value(album, &community))
        .collect();
    let mut reply = Map::new();
    reply.insert(
        "list".to_string(),
        json!({
            "catalog_set": found.catalog_set,
            "computed_at": unix_seconds(clock.now()),
            "seeds": 0,
            "candidates": found.albums,
            "artist": named(&found.artist),
        }),
    );
    reply.insert("albums".to_string(), Value::Array(albums));
    reply.insert("artists".to_string(), Value::Null);
    reply.insert("more".to_string(), json!(found.page.more));
    reply.insert("pending".to_string(), json!(found.pending));
    Ok(Reply::ok(reply))
}

/// The community values stored for `representatives`, those there are, by release id.
fn values(
    config: &Config,
    representatives: impl Iterator<Item = Option<u64>>,
) -> Result<HashMap<u64, Stored>, Refusal> {
    let ids: Vec<u64> = representatives.flatten().collect();
    let stored = community::stored(config, &ids).map_err(internal)?;
    Ok(ids.into_iter().zip(stored).collect())
}

/// A scored list's album in the contract's shape.
fn album_value(album: &Listed, community: &HashMap<u64, Stored>, detail: &Detail) -> Value {
    let reasons: Vec<Value> = album
        .scored
        .reasons
        .iter()
        .map(|reason| reason_value(reason.weight, &reason.connection))
        .collect();
    Shown {
        candidate: album.scored.candidate,
        representative: album.representative,
        rank: album.rank,
        score: Some(album.scored.score),
        artists: &album.artists,
        title: &album.title,
        year: album.year,
        label: album.label.as_ref(),
        styles: &album.styles,
        reasons,
        detail,
    }
    .value(community)
}

/// A discography's album in the contract's shape: no score and no reasons.
fn discography_value(album: &discography::Album, community: &HashMap<u64, Stored>) -> Value {
    Shown {
        candidate: album.candidate,
        representative: album.representative,
        rank: album.rank,
        score: None,
        artists: &album.artists,
        title: &album.title,
        year: album.year,
        label: album.label.as_ref(),
        styles: &album.styles,
        reasons: Vec::new(),
        detail: &album.detail,
    }
    .value(community)
}

/// What an album of any list shows.
struct Shown<'a> {
    candidate: Candidate,
    representative: Option<u64>,
    rank: u64,
    score: Option<f64>,
    artists: &'a [Named],
    title: &'a str,
    year: Option<u16>,
    label: Option<&'a Named>,
    styles: &'a [Named],
    reasons: Vec<Value>,
    detail: &'a Detail,
}

impl Shown<'_> {
    /// The album in the contract's shape; its community values are `/v1/community`'s row
    /// for its representative release in `community`, `null` when it has none.
    fn value(self, community: &HashMap<u64, Stored>) -> Value {
        let (kind, id) = self.candidate.kind_id();
        let community = self
            .representative
            .and_then(|id| community.get(&id).map(|stored| release(id, *stored)));
        json!({
            "kind": kind.name(),
            "id": id,
            "release_id": self.representative,
            "rank": self.rank,
            "score": self.score,
            "artists": self.artists.iter().map(named).collect::<Vec<_>>(),
            "title": self.title,
            "year": self.year,
            "label": self.label.map(named),
            "styles": self.styles.iter().map(named).collect::<Vec<_>>(),
            "community": community,
            "reasons": self.reasons,
            "formats": self.detail.formats,
            "in_library": self.detail.in_library,
            "mark": self.detail.mark.map(Verdict::name),
        })
    }
}

/// A reason in the contract's shape: its words, and the structure they are made from, each
/// field `null` or empty where its kind has none.
fn reason_value(weight: f64, connection: &Connection) -> Value {
    let mut fields = json!({
        "kind": connection.kind().name(),
        "words": reason::words(connection),
        "weight": weight,
        "artist": null,
        "role": null,
        "seed_roles": [],
        "label": null,
        "via": null,
        "relation": null,
        "of": null,
        "albums": null,
        "styles": [],
    });
    let set = |fields: &mut Value, key: &str, value: Value| fields[key] = value;
    match connection {
        Connection::Artist { artist, albums } => {
            set(&mut fields, "artist", named(artist));
            set(&mut fields, "albums", json!(albums));
        }
        Connection::Credit {
            artist,
            role,
            seed_roles,
            albums,
        } => {
            set(&mut fields, "artist", named(artist));
            set(&mut fields, "role", json!(role));
            set(&mut fields, "seed_roles", json!(seed_roles));
            set(&mut fields, "albums", json!(albums));
        }
        Connection::Label { label, via, albums } => {
            set(&mut fields, "label", named(label));
            set(&mut fields, "via", via.as_ref().map_or(Value::Null, named));
            set(&mut fields, "albums", json!(albums));
        }
        Connection::Related {
            artist,
            relation,
            of,
        } => {
            set(&mut fields, "artist", named(artist));
            set(&mut fields, "relation", json!(relation.name()));
            set(&mut fields, "of", named(of));
        }
        Connection::Styles { styles } => {
            set(
                &mut fields,
                "styles",
                Value::Array(styles.iter().map(named).collect()),
            );
        }
    }
    fields
}

/// An artist, label or style by id and name, the name `null` when the catalog has none.
pub(crate) fn named(named: &Named) -> Value {
    json!({"id": named.id, "name": named.name})
}

/// A list that can't be made, as its `ok: false` code; a failure to read is a 500.
fn refused(error: &ListError) -> Result<Reply, Refusal> {
    let message = error.to_string();
    let mut fields = Map::new();
    let code = match error {
        ListError::NoCatalog | ListError::OtherCatalog => "no_catalog",
        ListError::NoProfile => "no_profile",
        ListError::Busy => return Err(Refusal::Busy),
        ListError::NoList => "no_list",
        ListError::Stale { rebuilding } => {
            fields.insert("rebuilding".to_string(), json!(*rebuilding));
            "out_of_date"
        }
        ListError::NotInProfile(_)
        | ListError::NotInCatalog(_)
        | ListError::NoAlbumBy(_)
        | ListError::NoSeedBy(_) => "no_seed",
        ListError::UnknownArtist(_) => return Err(Refusal::UnknownArtist(message)),
        ListError::VariousArtists => return Err(Refusal::VariousArtists(message)),
        ListError::Catalog(_)
        | ListError::Profile(_)
        | ListError::Rescore(_)
        | ListError::Feedback(_)
        | ListError::Community(_) => return Err(internal(message)),
    };
    Ok(Reply::refused(code, &message, fields))
}
