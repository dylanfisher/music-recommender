//! `POST /v1/sync/status` (docs/api.md, Sync status): `features::status`, the values
//! `sync status` prints and the status page shows, as JSON for the player's settings.

use config::Config;
use features::clock::Clock;
use features::status::{self, Found, Status, SyncStatus};
use serde_json::{Map, Value, json};

use crate::body::{BODY, internal, only};
use crate::reply::{Refusal, Reply};

/// Answers with the status read at the clock's time. Nothing that can't be read fails it,
/// but the community database and the sync lock file: those are a 500.
pub(crate) fn answer(
    body: &Map<String, Value>,
    config: &Config,
    clock: &dyn Clock,
) -> Result<Reply, Refusal> {
    only(body, BODY, &[])?;
    let status = status::read(config, clock).map_err(internal)?;
    let mut reply = Map::new();
    reply.insert("at".to_string(), json!(status.at));
    reply.insert("sync".to_string(), sync(&status.sync));
    reply.insert("dumps".to_string(), dumps(&status));
    Ok(Reply::ok(reply))
}

fn sync(sync: &SyncStatus) -> Value {
    let p = &sync.progress;
    let counts = |c: features::status::Counts| {
        json!({"queued": c.queued, "with_values": c.with_values, "failed": c.failed,
               "gone": c.gone, "due": c.due})
    };
    json!({
        "paused": p.paused,
        "running": sync.running,
        "other_shape": sync.other_shape,
        "yielding_since": p.yielding_since,
        "library": counts(p.library),
        "candidates": counts(p.candidates),
        "failures": p.failures.iter()
            .map(|(release_id, error)| json!({"release_id": release_id, "error": error}))
            .collect::<Vec<_>>(),
        "requests_last_hour": sync.requests,
        "last_error": p.last_error.as_ref()
            .map(|(at, message)| json!({"at": at, "message": message})),
        "seconds_left": sync.seconds_left(),
        "requests_per_minute": status::REQUESTS_PER_MINUTE,
    })
}

fn dumps(status: &Status) -> Value {
    let dumps = &status.dumps;
    let (newest_set, newest_set_error) = split(&dumps.newest_set);
    let (running, running_error) = split(&dumps.running);
    let records: Vec<Value> = dumps
        .rows()
        .iter()
        .map(|row| {
            json!({
                "dump": row.dump,
                "building_records": row.building.map(|(records, _)| records),
                "building_loaded": row.building.map(|(_, loaded)| loaded),
                "catalog_records": row.in_catalog,
            })
        })
        .collect();
    json!({
        "dir": dumps.dir.as_ref().map(|dir| dir.display().to_string()),
        "newest_set": newest_set,
        "newest_set_error": newest_set_error,
        "running": running,
        "running_error": running_error,
        "catalog": found(&dumps.catalog_found()),
        "build": found(&dumps.build_found()),
        "records": records,
    })
}

/// A value and its error: one of the two is `null`.
fn split<T: Clone + Into<Value>>(result: &Result<T, String>) -> (Value, Value) {
    match result {
        Ok(value) => (value.clone().into(), Value::Null),
        Err(error) => (Value::Null, json!(error)),
    }
}

/// A catalog or a build: `state` is `none`, `other_shape`, `present` or `error`.
fn found(found: &Found) -> Value {
    let (state, date, swapped_at, error) = match found {
        Found::None => ("none", None, None, None),
        Found::OtherShape => ("other_shape", None, None, None),
        Found::Present { date, swapped_at } => ("present", date.as_deref(), *swapped_at, None),
        Found::Unreadable(error) => ("error", None, None, Some(error)),
    };
    json!({"state": state, "date": date, "swapped_at": swapped_at, "error": error})
}
