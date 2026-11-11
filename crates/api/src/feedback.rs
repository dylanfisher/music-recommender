//! The feedback routes (docs/api.md, Feedback): a verdict on a release or master, its undo,
//! and the list of them, read strictly and answered in the contract's shape.

use config::Config;
use features::clock::{Clock, unix_seconds};
use features::feedback::{Feedback, Kind, Mark, Verdict};
use serde_json::{Map, Value, json};

use crate::body::{BODY, bad, integer, internal, only, required, string};
use crate::reply::{Refusal, Reply};

/// The feedback routes; their paths are in `route.rs`'s table.
#[derive(Clone, Copy)]
pub(crate) enum Route {
    /// `POST /v1/feedback`: sets a verdict.
    Set,
    /// `POST /v1/feedback/undo`: removes one.
    Undo,
    /// `POST /v1/feedback/list`: a page of them.
    List,
}

/// Answers a feedback route with its JSON body.
pub(crate) fn answer(
    route: Route,
    body: &Map<String, Value>,
    config: &Config,
    clock: &dyn Clock,
) -> Result<Reply, Refusal> {
    let mut store = Feedback::open(&config.data_dir).map_err(internal)?;
    let mut reply = Map::new();
    match route {
        Route::Set => {
            only(body, BODY, &["kind", "id", "verdict"])?;
            let (kind, id) = target(body)?;
            let name = string(required(body, "verdict")?, "verdict")?;
            let verdict =
                Verdict::parse(&name).ok_or_else(|| bad(&format!("verdict can't be {name:?}")))?;
            let now = unix_seconds(clock.now());
            let mark = store.set(kind, id, verdict, now).map_err(internal)?;
            reply.insert("feedback".to_string(), mark_value(&mark));
        }
        Route::Undo => {
            only(body, BODY, &["kind", "id"])?;
            let (kind, id) = target(body)?;
            let removed = store.undo(kind, id).map_err(internal)?;
            reply.insert("removed".to_string(), json!(removed));
        }
        Route::List => {
            only(body, BODY, &["after"])?;
            let after = integer(required(body, "after")?, "after", 0)?;
            let page = store.list(after).map_err(internal)?;
            let marks: Vec<Value> = page.marks.iter().map(mark_value).collect();
            reply.insert("feedback".to_string(), Value::Array(marks));
            reply.insert("more".to_string(), json!(page.more));
        }
    }
    Ok(Reply::ok(reply))
}

/// The `kind` and `id` a verdict is on.
fn target(body: &Map<String, Value>) -> Result<(Kind, i64), Refusal> {
    let name = string(required(body, "kind")?, "kind")?;
    let kind = Kind::parse(&name).ok_or_else(|| bad(&format!("kind can't be {name:?}")))?;
    Ok((kind, integer(required(body, "id")?, "id", 1)?))
}

/// A mark in the contract's shape.
fn mark_value(mark: &Mark) -> Value {
    json!({
        "seq": mark.seq,
        "kind": mark.kind.name(),
        "id": mark.id,
        "verdict": mark.verdict.name(),
        "at": mark.at,
    })
}
