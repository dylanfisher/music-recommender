//! `POST /v1/profile` (docs/api.md, Profile): the owner's library, read, kept and queued for
//! the community sync as `profile load` does with a file. A profile kept asks for the
//! scores' rebuild, which runs on `serve`'s scores thread, not this one, and rebuilds only
//! if what the score reads from the profile changed; the reply doesn't wait for it
//! (ADRs 0024, 0025, 0026).

use config::Config;
use features::profile;
use features::rescore::Requests;
use serde_json::{Map, json};

use crate::body::internal;
use crate::reply::{Refusal, Reply};

/// Keeps the profile in `body`, asks `rescore` for a rebuild, and answers with what queueing
/// it did. The server has already capped the body at `profile::MAX_BYTES`.
pub(crate) fn answer(body: &[u8], config: &Config, rescore: &Requests) -> Result<Reply, Refusal> {
    let profile =
        profile::parse(body).map_err(|refused| Refusal::BadRequest(refused.to_string()))?;
    // Queue, keep, then ask for the rebuild: a request that fails leaves no kept profile and
    // no rebuild asked for.
    let queued = profile::queue(&profile, config).map_err(internal)?;
    profile::keep(&profile, &config.data_dir).map_err(internal)?;
    rescore.request();
    let mut reply = Map::new();
    reply.insert("albums".to_string(), json!(queued.albums));
    reply.insert("with_release_id".to_string(), json!(queued.with_id));
    reply.insert("queued".to_string(), json!(queued.added));
    Ok(Reply::ok(reply))
}
