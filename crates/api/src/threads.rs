//! `POST /v1/threads` (docs/api.md, Threads): the library's albums connected to one of them,
//! through `features::rediscover::threads`, from the seeds' read Rediscover keeps (ADR
//! 0035). It reads no seed list, so it takes none of the seed lists' slots.

use config::Config;
use features::clock::Clock;
use features::profile::MAX_KEY_CHARS;
use features::rediscover::{self, Connected, Kept, Threads, ThreadsError};
use serde_json::{Map, Value};

use crate::body::{BODY, bad, internal, only, required, string};
use crate::rediscover::{album_value, thread_value};
use crate::reply::{Refusal, Reply};

/// Answers with the threads of the album whose `key` the body names.
pub(crate) fn answer(
    body: &Map<String, Value>,
    config: &Config,
    clock: &dyn Clock,
    kept: &Kept,
) -> Result<Reply, Refusal> {
    only(body, BODY, &["key"])?;
    let key = string(required(body, "key")?, "key")?;
    if key.is_empty() || key.chars().count() > MAX_KEY_CHARS {
        return Err(bad(&format!(
            "key must be from 1 to {MAX_KEY_CHARS} characters"
        )));
    }
    match rediscover::threads(config, clock, kept, &key) {
        Ok(found) => Ok(Reply::ok(threads_value(&found))),
        Err(error) => {
            let code = match error {
                ThreadsError::NoProfile => "no_profile",
                ThreadsError::NoSeed(_) => "no_seed",
                ThreadsError::Untagged(_) => "untagged",
                ThreadsError::NoCatalog | ThreadsError::OtherCatalog => "no_catalog",
                ThreadsError::Profile(_)
                | ThreadsError::Catalog(_)
                | ThreadsError::Community(_) => return Err(internal(error)),
            };
            Ok(Reply::refused(code, &error.to_string(), Map::new()))
        }
    }
}

/// The threads in the contract's shape.
fn threads_value(found: &Threads) -> Map<String, Value> {
    let mut reply = Map::new();
    reply.insert("seed".to_string(), connected_value(&found.seed));
    reply.insert("computed_at".to_string(), found.computed_at.into());
    reply.insert(
        "albums".to_string(),
        Value::Array(found.albums.iter().map(connected_value).collect()),
    );
    reply.insert(
        "threads".to_string(),
        Value::Array(found.threads.iter().map(thread_value).collect()),
    );
    reply
}

fn connected_value(connected: &Connected) -> Value {
    Value::Object(album_value(
        connected.id,
        &connected.album,
        connected.year,
        connected.label.as_ref(),
        &connected.styles,
        connected.community,
    ))
}
