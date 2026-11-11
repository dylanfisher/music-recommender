//! `POST /v1/profile` (docs/api.md, Profile): the owner's library, read, kept and queued for
//! the community sync as `profile load` does with a file. Nothing scores it yet, so nothing
//! is computed again after it (ADRs 0024, 0025).

use config::Config;
use features::profile;
use serde_json::{Map, json};

use crate::body::internal;
use crate::reply::{Refusal, Reply};

/// Keeps the profile in `body` and answers with what queueing it did. The server has already capped the
/// body at `profile::MAX_BYTES`.
pub(crate) fn answer(body: &[u8], config: &Config) -> Result<Reply, Refusal> {
    let profile =
        profile::parse(body).map_err(|refused| Refusal::BadRequest(refused.to_string()))?;
    profile::keep(&profile, &config.data_dir).map_err(internal)?;
    let queued = profile::queue(&profile, config).map_err(internal)?;
    let mut reply = Map::new();
    reply.insert("albums".to_string(), json!(queued.albums));
    reply.insert("with_release_id".to_string(), json!(queued.with_id));
    reply.insert("queued".to_string(), json!(queued.added));
    Ok(Reply::ok(reply))
}
