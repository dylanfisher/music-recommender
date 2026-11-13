//! `POST /v1/rediscover`, `/v1/rediscover/not_now` and `/v1/rediscover/not_now/undo`
//! (docs/api.md, Rediscover): the day's albums of the library to play again, through
//! `features::rediscover`, and the marks that leave one out for a while (ADR 0034).

use config::Config;
use features::clock::{Clock, unix_seconds};
use features::rediscover::{self, Day, Id, Kept, Reason, Rediscover, RediscoverError};
use serde_json::{Map, Value, json};

use crate::body::{BODY, bad, integer, internal, only, required, string};
use crate::community::release;
use crate::recommendations::named;
use crate::reply::{Refusal, Reply};

/// The Rediscover routes; their paths are in `route.rs`'s table.
#[derive(Clone, Copy)]
pub(crate) enum Route {
    /// `POST /v1/rediscover`: the day's albums.
    Day,
    /// `POST /v1/rediscover/not_now`: leaves an album out for some days.
    NotNow,
    /// `POST /v1/rediscover/not_now/undo`: removes that.
    Undo,
}

/// The most days a not-now mark lasts.
const MAX_DAYS: i64 = 365;

/// Seconds in a day.
const DAY: i64 = 86_400;

/// Answers a Rediscover route with its JSON body, the seeds' read kept in `kept`.
pub(crate) fn answer(
    route: Route,
    body: &Map<String, Value>,
    config: &Config,
    clock: &dyn Clock,
    kept: &Kept,
) -> Result<Reply, Refusal> {
    let mut reply = Map::new();
    match route {
        Route::Day => {
            only(body, BODY, &["day"])?;
            let text = string(required(body, "day")?, "day")?;
            let day = Day::parse(&text)
                .ok_or_else(|| bad(&format!("day must be a real date, YYYY-MM-DD: {text:?}")))?;
            return match rediscover::rediscover(config, clock, kept, &day) {
                Ok(found) => Ok(Reply::ok(day_value(&found))),
                Err(RediscoverError::NoProfile) => Ok(Reply::refused(
                    "no_profile",
                    &RediscoverError::NoProfile.to_string(),
                    Map::new(),
                )),
                Err(error) => Err(internal(error)),
            };
        }
        Route::NotNow => {
            only(body, BODY, &["id", "days"])?;
            let id = id(body)?;
            let days = integer(required(body, "days")?, "days", 1)?;
            if days > MAX_DAYS {
                return Err(bad(&format!("days must be from 1 to {MAX_DAYS}")));
            }
            let now = unix_seconds(clock.now());
            let until = now + days * DAY;
            rediscover::not_now(config, id, until, now).map_err(internal)?;
            reply.insert("until".to_string(), json!(until));
        }
        Route::Undo => {
            only(body, BODY, &["id"])?;
            let removed = rediscover::not_now_undo(config, id(body)?).map_err(internal)?;
            reply.insert("removed".to_string(), json!(removed));
        }
    }
    Ok(Reply::ok(reply))
}

/// The album `id` a mark is on.
fn id(body: &Map<String, Value>) -> Result<Id, Refusal> {
    let text = string(required(body, "id")?, "id")?;
    Id::parse(&text).ok_or_else(|| {
        bad(&format!(
            "id must be r and a release id, or k and 16 hex digits: {text:?}"
        ))
    })
}

/// The day's albums in the contract's shape.
fn day_value(found: &Rediscover) -> Map<String, Value> {
    let albums: Vec<Value> = found
        .albums
        .iter()
        .map(|surfaced| {
            let album = &surfaced.album;
            let community = album
                .discogs_release_id
                .zip(surfaced.community)
                .map(|(id, stored)| release(id, stored));
            json!({
                "id": surfaced.id.to_string(),
                "key": album.key,
                "artist": album.artist,
                "title": album.title,
                "release_id": album.discogs_release_id,
                "year": surfaced.year,
                "label": surfaced.label.as_ref().map(named),
                "styles": surfaced.styles.iter().map(named).collect::<Vec<_>>(),
                "plays": album.plays,
                "skips": album.skips,
                "last_played": album.last_played,
                "score": surfaced.score,
                "community": community,
                "reasons": surfaced.reasons.iter().map(reason_value).collect::<Vec<_>>(),
            })
        })
        .collect();
    let threads: Vec<Value> = found
        .threads
        .iter()
        .map(|thread| {
            json!({
                "kind": thread.kind.name(),
                "words": thread.words,
                "ids": thread.ids.iter().map(Id::to_string).collect::<Vec<_>>(),
                "artist": thread.artist.as_ref().map(named),
                "label": thread.label.as_ref().map(named),
            })
        })
        .collect();
    let mut reply = Map::new();
    reply.insert(
        "rediscover".to_string(),
        json!({
            "day": found.day.as_str(),
            "computed_at": found.computed_at,
            "albums_considered": found.albums_considered,
        }),
    );
    reply.insert("albums".to_string(), Value::Array(albums));
    reply.insert("threads".to_string(), Value::Array(threads));
    reply
}

/// A reason in the contract's shape, each field `null` or empty where its kind has none.
fn reason_value(reason: &Reason) -> Value {
    let (keys, artist, role, label) = match reason {
        Reason::Link(link) => (
            link.keys.clone(),
            link.artist.as_ref().map(named),
            link.role.clone(),
            link.label.as_ref().map(named),
        ),
        _ => (Vec::new(), None, None, None),
    };
    json!({
        "kind": reason.kind(),
        "words": reason.words(),
        "keys": keys,
        "artist": artist,
        "role": role,
        "label": label,
    })
}
