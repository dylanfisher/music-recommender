//! `POST /v1/community` (docs/api.md, Community values): the values the sync stored for a
//! list of release ids, for the player's library columns, and why an id has none.

use std::collections::HashSet;

use config::Config;
use features::community::{self, Stored, Values};
use serde_json::{Map, Value, json};

use crate::body::{BODY, bad, integer, internal, only, required};
use crate::reply::{Refusal, Reply};

/// The most release ids in one request. A reply is about 200 bytes an id, so 1,000 stay
/// well under the player's 1 MB cap on a reply.
const MAX_IDS: usize = 1000;

/// Answers with each release id's stored values, in the order asked, each id once.
pub(crate) fn answer(body: &Map<String, Value>, config: &Config) -> Result<Reply, Refusal> {
    only(body, BODY, &["release_ids"])?;
    let Value::Array(items) = required(body, "release_ids")? else {
        return Err(bad("release_ids must be an array"));
    };
    if items.len() > MAX_IDS {
        return Err(bad(&format!("release_ids holds more than {MAX_IDS}")));
    }
    let mut seen = HashSet::new();
    let mut ids = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let id = integer(item, &format!("release_ids[{index}]"), 1)?.cast_unsigned();
        if seen.insert(id) {
            ids.push(id);
        }
    }
    let stored = community::stored(config, &ids).map_err(internal)?;
    let releases: Vec<Value> = ids
        .iter()
        .zip(stored)
        .map(|(&id, stored)| release(id, stored))
        .collect();
    let mut reply = Map::new();
    reply.insert("releases".to_string(), Value::Array(releases));
    Ok(Reply::ok(reply))
}

/// One release in the contract's shape, which `/v1/recommendations` sends for each album's
/// representative too.
pub(crate) fn release(release_id: u64, stored: Stored) -> Value {
    let (status, checked_at, values) = match stored {
        Stored::NotQueued => ("not_queued", None, None),
        Stored::Queued => ("queued", None, None),
        Stored::Fetched { values, at } => ("fetched", Some(at), Some(values)),
        Stored::Failed { values, at } => ("failed", Some(at), values),
        Stored::Gone { at } => ("gone", Some(at), None),
    };
    json!({
        "release_id": release_id,
        "status": status,
        "checked_at": checked_at,
        "values": values.map(|v: Values| json!({
            "rating_average": v.rating_average,
            "rating_count": v.rating_count,
            "have": v.have,
            "want": v.want,
            "num_for_sale": v.num_for_sale,
            "lowest_price": v.lowest_price,
        })),
    })
}
