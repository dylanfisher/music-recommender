//! `POST /v1/recommendations` (docs/api.md, Recommendations): a page of a list's albums or
//! artists, the whole library's or a seed album's or artist's, filtered, read strictly and
//! answered in the contract's shape through `features::list`, which `recommend` prints
//! through too (ADR 0028).

use std::collections::HashMap;

use config::Config;
use features::clock::Clock;
use features::community::{self, Stored};
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
const FIELDS: [&str; 6] = ["list", "release_id", "artist", "show", "offset", "limit"];

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

/// The request in `body`, every field checked.
fn asked(body: &Map<String, Value>) -> Result<Asked, Refusal> {
    only(
        body,
        BODY,
        &[FIELDS.as_slice(), FILTERS.as_slice(), &[SORT]].concat(),
    )?;
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
    Ok(Asked {
        of,
        show,
        offset,
        limit,
    })
}

/// The albums' order in `body`, by score unless sent.
fn sort(body: &Map<String, Value>) -> Result<Sort, Refusal> {
    match body.get(SORT) {
        None => Ok(Sort::Score),
        Some(value) => {
            let name = string(value, SORT)?;
            Sort::named(&name).ok_or_else(|| bad(&format!("sort can't be {name:?}")))
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
        }),
    );
    let (albums, artists, more) = match &asked.show {
        Show::Albums(filter, missing, sort) => {
            let page = list
                .albums(filter, *missing, *sort, asked.offset, asked.limit)
                .map_err(internal)?;
            let community = values(config, page.items.iter())?;
            let albums: Vec<Value> = page
                .items
                .iter()
                .map(|album| album_value(album, &community))
                .collect();
            (Value::Array(albums), Value::Null, page.more)
        }
        Show::Artists => {
            let page = list.artists(asked.offset, asked.limit).map_err(internal)?;
            let community = values(config, page.items.iter().map(|artist| &artist.best))?;
            let artists: Vec<Value> = page
                .items
                .iter()
                .map(|artist| {
                    json!({
                        "rank": artist.rank,
                        "id": artist.id,
                        "name": artist.name,
                        "score": artist.score,
                        "albums": artist.albums,
                        "best": album_value(&artist.best, &community),
                    })
                })
                .collect();
            (Value::Null, Value::Array(artists), page.more)
        }
    };
    reply.insert("albums".to_string(), albums);
    reply.insert("artists".to_string(), artists);
    reply.insert("more".to_string(), json!(more));
    Ok(Reply::ok(reply))
}

/// The community values stored for the representatives of `albums`, by release id.
fn values<'a>(
    config: &Config,
    albums: impl Iterator<Item = &'a Listed>,
) -> Result<HashMap<u64, Stored>, Refusal> {
    let ids: Vec<u64> = albums.filter_map(|album| album.representative).collect();
    let stored = community::stored(config, &ids).map_err(internal)?;
    Ok(ids.into_iter().zip(stored).collect())
}

/// An album in the contract's shape; its community values are `/v1/community`'s row for its
/// representative release, `null` when it has none.
fn album_value(album: &Listed, community: &HashMap<u64, Stored>) -> Value {
    let (kind, id) = album.scored.candidate.kind_id();
    let community = album
        .representative
        .and_then(|id| community.get(&id).map(|stored| release(id, *stored)));
    let reasons: Vec<Value> = album
        .scored
        .reasons
        .iter()
        .map(|reason| reason_value(reason.weight, &reason.connection))
        .collect();
    json!({
        "kind": kind.name(),
        "id": id,
        "release_id": album.representative,
        "rank": album.rank,
        "score": album.scored.score,
        "artists": album.artists.iter().map(named).collect::<Vec<_>>(),
        "title": album.title,
        "year": album.year,
        "label": album.label.as_ref().map(named),
        "styles": album.styles.iter().map(named).collect::<Vec<_>>(),
        "community": community,
        "reasons": reasons,
    })
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
        ListError::Catalog(_)
        | ListError::Profile(_)
        | ListError::Rescore(_)
        | ListError::Feedback(_)
        | ListError::Community(_) => return Err(internal(message)),
    };
    Ok(Reply::refused(code, &message, fields))
}
