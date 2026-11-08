//! The downloads routes (docs/api.md, Downloads): their requests read strictly, the job in the
//! contract's shape, and the store's refusals as the contract's error codes. The store makes
//! every decision, the lease and claim checks included; this module only maps.

use std::collections::HashSet;
use std::time::SystemTime;

use config::Config;
use features::clock::unix_seconds;
use features::download::{
    Downloads, Event, Handoff, Hold, Job, NewJob, Policy, Refusal as Refused, Release, Source,
    Stage, State, Tagging, TaggingStatus, Track, Upgrade,
};
use serde_json::{Map, Value, json};

use crate::reply::{Refusal, Reply};

/// The longest string field, in characters, unless a field says otherwise.
const MAX_STRING_CHARS: usize = 1000;
/// A snapshot has one or two tracklists of 1 to 500 tracks.
const MAX_TRACKLISTS: usize = 2;
const MAX_TRACKS: usize = 500;
/// An upgrade replaces 1 to 500 old files.
const MAX_OLD_PATHS: usize = 500;
/// A machine's name is 1 to 64 characters.
const MAX_MACHINE_CHARS: usize = 64;
/// A `list` reply stops before its jobs pass this many bytes, under the player's 1 MB cap.
const MAX_PAGE_BYTES: usize = 768 * 1024;

/// Why the downloads are off, for `config` and the `downloads_off` code.
const OFF_REASON: &str = "the service's config has no [slskd] and [staging] tables";

/// The routes under `/v1/downloads/`.
pub(crate) const ROUTES: [&str; 6] = ["config", "list", "create", "retry", "cancel", "handoff"];

/// Answers `POST /v1/downloads/<route>` with its JSON body.
pub(crate) fn answer(
    route: &str,
    body: &Map<String, Value>,
    config: &Config,
) -> Result<Reply, Refusal> {
    if route == "config" {
        only(body, "the body", &[])?;
        return Ok(settings(config));
    }
    if config.downloads.is_none() {
        return Ok(Reply::refused("downloads_off", OFF_REASON, Map::new()));
    }
    let mut store = Downloads::open(&config.data_dir).map_err(internal)?;
    let now = unix_seconds(SystemTime::now());
    match route {
        "list" => list(body, &store, now),
        "create" => create(body, &mut store, now),
        "retry" => retry(body, &mut store, now),
        "cancel" => {
            only(body, "the body", &["id"])?;
            let id = id(body)?;
            let cancelled = store.transition(id, Event::Cancel, now).map_err(internal)?;
            Ok(answered(cancelled))
        }
        "handoff" => handoff(body, &mut store, now),
        _ => Err(Refusal::NotFound),
    }
}

/// `config`: whether the downloads are on.
fn settings(config: &Config) -> Reply {
    let mut reply = Map::new();
    let staging = config.downloads.as_ref().map(|settings| {
        settings
            .staging_dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
    });
    reply.insert("enabled".to_string(), Value::Bool(staging.is_some()));
    reply.insert(
        "off_reason".to_string(),
        json!(staging.is_none().then_some(OFF_REASON)),
    );
    let policies: Vec<&str> = Policy::ALL.iter().map(|policy| policy.name()).collect();
    reply.insert("format_policies".to_string(), json!(policies));
    reply.insert(
        "default_format_policy".to_string(),
        json!(Policy::DEFAULT.name()),
    );
    reply.insert("staging_dir_name".to_string(), json!(staging.flatten()));
    Reply::ok(reply)
}

/// `list`: the jobs changed after a revision, a page at a time.
fn list(body: &Map<String, Value>, store: &Downloads, now: i64) -> Result<Reply, Refusal> {
    only(body, "the body", &["epoch", "after"])?;
    let epoch = match required(body, "epoch")? {
        Value::Null => None,
        Value::String(epoch) => Some(epoch.as_str()),
        _ => return Err(bad("epoch must be a string or null")),
    };
    let after = integer(required(body, "after")?, "after", 0)?;
    let listing = store.list(epoch, after, now).map_err(internal)?;
    let mut jobs = Vec::new();
    let mut bytes = 0;
    let mut more = listing.more;
    for job in &listing.jobs {
        let value = job_value(job);
        let size = value.to_string().len();
        if !jobs.is_empty() && bytes + size > MAX_PAGE_BYTES {
            more = true;
            break;
        }
        bytes += size;
        jobs.push(value);
    }
    let mut reply = Map::new();
    reply.insert("epoch".to_string(), json!(listing.epoch));
    reply.insert("revision".to_string(), json!(listing.revision));
    reply.insert("reset".to_string(), json!(listing.reset));
    reply.insert("more".to_string(), json!(more));
    reply.insert("jobs".to_string(), Value::Array(jobs));
    Ok(Reply::ok(reply))
}

/// `create`: queues a release from its snapshot.
fn create(body: &Map<String, Value>, store: &mut Downloads, now: i64) -> Result<Reply, Refusal> {
    only(
        body,
        "the body",
        &["snapshot", "format_policy", "source", "upgrade"],
    )?;
    let (release, tracklists) =
        snapshot(required(body, "snapshot")?).map_err(Refusal::BadRequest)?;
    let format_policy = match body.get("format_policy") {
        None => Policy::DEFAULT,
        Some(value) => policy(value, "format_policy")?,
    };
    let source = string(required(body, "source")?, "source")?;
    let source =
        Source::parse(&source).ok_or_else(|| bad(&format!("source can't be {source:?}")))?;
    let upgrade = match (source, body.get("upgrade").filter(|value| !value.is_null())) {
        (Source::Upgrade, Some(value)) => Some(upgrade(value)?),
        (Source::Upgrade, None) => return Err(bad("an upgrade needs `upgrade`")),
        (_, Some(_)) => return Err(bad("only an upgrade takes `upgrade`")),
        (_, None) => None,
    };
    let job = NewJob {
        release,
        tracklists,
        format_policy,
        source,
        upgrade,
    };
    let created = store.create(&job, now).map_err(internal)?;
    Ok(answered(unlossy(created)?))
}

/// `retry`: back to the state the failed stage resumes from.
fn retry(body: &Map<String, Value>, store: &mut Downloads, now: i64) -> Result<Reply, Refusal> {
    only(body, "the body", &["id", "format_policy"])?;
    let id = id(body)?;
    let format_policy = body
        .get("format_policy")
        .map(|value| policy(value, "format_policy"))
        .transpose()?;
    let retried = store.retry(id, format_policy, now).map_err(internal)?;
    Ok(answered(unlossy(retried)?))
}

/// `handoff`: the player's reports and claims.
fn handoff(body: &Map<String, Value>, store: &mut Downloads, now: i64) -> Result<Reply, Refusal> {
    only(body, "the body", &["id", "machine", "event"])?;
    let id = id(body)?;
    let machine = string(required(body, "machine")?, "machine")?;
    let length = machine.chars().count();
    if length == 0 || length > MAX_MACHINE_CHARS {
        return Err(bad(&format!(
            "machine must be 1 to {MAX_MACHINE_CHARS} characters"
        )));
    }
    let event = object(required(body, "event")?, "event")?;
    let kind = string(required(event, "type")?, "event.type")?;
    let fields: &[&str] = match kind.as_str() {
        "tagging" => &["type", "status", "reasons"],
        "claim" => &["type", "take_over"],
        kind if kind == failed() => &["type", "stage", "reason"],
        _ => &["type"],
    };
    only(event, "event", fields)?;
    let handoff = match kind.as_str() {
        "lease" => Handoff::Lease,
        "release_lease" => Handoff::ReleaseLease,
        "tagging" => {
            let status = string(required(event, "status")?, "event.status")?;
            let status = TaggingStatus::parse(&status)
                .ok_or_else(|| bad(&format!("event.status can't be {status:?}")))?;
            let reasons = strings(required(event, "reasons")?, "event.reasons", usize::MAX)?;
            Handoff::Tagging(Tagging { status, reasons })
        }
        "claim" => Handoff::Claim {
            take_over: match event.get("take_over") {
                None => false,
                Some(Value::Bool(take_over)) => *take_over,
                Some(_) => return Err(bad("event.take_over must be true or false")),
            },
        },
        "release" => Handoff::Release,
        // The player's lifecycle events are named for the state each reports.
        kind if kind == State::Approved.name() => Handoff::Approved,
        kind if kind == State::Importing.name() => Handoff::Importing,
        kind if kind == State::Imported.name() => Handoff::Imported,
        kind if kind == State::Rejected.name() => Handoff::Rejected,
        kind if kind == failed() => {
            let name = string(required(event, "stage")?, "event.stage")?;
            let stage = Stage::parse(&name)
                .filter(|stage| {
                    matches!(
                        stage,
                        Stage::Tag | Stage::Move | Stage::Delete | Stage::Carry
                    )
                })
                .ok_or_else(|| bad(&format!("the player can't report a {name:?} failure")))?;
            let reason = string(required(event, "reason")?, "event.reason")?;
            Handoff::Failed { stage, reason }
        }
        _ => return Err(bad(&format!("event.type can't be {kind:?}"))),
    };
    let answer = store
        .handoff(id, &machine, &handoff, now)
        .map_err(internal)?;
    Ok(answered(answer))
}

/// The store's answer, with a lossy policy on an upgrade turned into a 400.
fn unlossy(answer: Result<Job, Refused>) -> Result<Result<Job, Refused>, Refusal> {
    match answer {
        Err(refusal @ Refused::LossyUpgrade(_)) => Err(bad(&refusal.to_string())),
        other => Ok(other),
    }
}

/// `{"ok": true, "job": ...}`, or the store's refusal as its code.
fn answered(answer: Result<Job, Refused>) -> Reply {
    let refusal = match answer {
        Ok(job) => {
            let mut reply = Map::new();
            reply.insert("job".to_string(), job_value(&job));
            return Reply::ok(reply);
        }
        Err(refusal) => refusal,
    };
    let message = refusal.to_string();
    let mut fields = Map::new();
    let code = match refusal {
        Refused::NoSuchJob => "no_such_job",
        Refused::Illegal(state) => {
            fields.insert("state".to_string(), json!(state.name()));
            "illegal_transition"
        }
        Refused::Claimed(hold) => {
            fields.insert("machine".to_string(), json!(hold.machine));
            fields.insert("expires_at".to_string(), json!(hold.expires_at));
            "claimed"
        }
        Refused::NotClaimed => "not_claimed",
        Refused::LossyUpgrade(_) => unreachable!("`unlossy` turns it into a 400 first"),
        Refused::AlreadyActive(job_id) => {
            fields.insert("job_id".to_string(), json!(job_id));
            "already_active"
        }
    };
    Reply::refused(code, &message, fields)
}

/// A job in the contract's shape (api.md, The job).
#[must_use]
pub fn job_value(job: &Job) -> Value {
    let hold = |hold: &Option<Hold>| {
        hold.as_ref()
            .map(|hold| json!({"machine": hold.machine, "expires_at": hold.expires_at}))
    };
    json!({
        "id": job.id,
        "revision": job.revision,
        "source": job.source.name(),
        "format_policy": job.format_policy.name(),
        "release": {
            "release_id": job.release.release_id,
            "artist": job.release.artist,
            "title": job.release.title,
            "year": job.release.year,
        },
        "state": job.state.name(),
        "failed_stage": job.state.failed_stage().map(Stage::name),
        "reason": job.reason,
        "attempt": job.attempt,
        "next_attempt_at": job.next_attempt_at,
        "progress": job.progress.map(|p| json!({
            "files_done": p.files_done,
            "files_total": p.files_total,
            "bytes_done": p.bytes_done,
            "bytes_total": p.bytes_total,
        })),
        "candidate": job.candidate.as_ref().map(|c| json!({
            "peer": c.peer,
            "format": c.format.name(),
            "bit_depth": c.bit_depth,
            "sample_rate": c.sample_rate,
            "bitrate": c.bitrate,
            "variable_bitrate": c.variable_bitrate,
            "file_count": c.file_count,
            "bytes": c.bytes,
            "tracklist": c.tracklist,
        })),
        "files": job.files.iter().map(|f| json!({
            "name": f.name, "size": f.size, "position": f.position,
        })).collect::<Vec<_>>(),
        "staged_path": job.staged_path,
        "tagging": {"status": job.tagging.status.name(), "reasons": job.tagging.reasons},
        "lease": hold(&job.lease),
        "review": hold(&job.review),
        "upgrade": job.upgrade.as_ref().map(|u| json!({
            "current_quality": u.current_quality, "old_paths": u.old_paths,
        })),
        "cleanup_pending": job.cleanup_pending,
        "created_at": job.created_at,
        "updated_at": job.updated_at,
    })
}

/// Reads a release snapshot's JSON text, as `create` takes its `snapshot`, for
/// `downloads add`.
///
/// # Errors
///
/// A line saying what isn't JSON, or naming the field that isn't the contract's.
pub fn read_snapshot(text: &str) -> Result<(Release, Vec<Vec<Track>>), String> {
    let value: Value =
        serde_json::from_str(text).map_err(|error| format!("the snapshot isn't JSON: {error}"))?;
    snapshot(&value)
}

/// Reads a release snapshot (api.md, `create`'s `snapshot`), or says what is wrong with it,
/// naming the field.
fn snapshot(value: &Value) -> Result<(Release, Vec<Vec<Track>>), String> {
    let words = |refusal: Refusal| match refusal {
        Refusal::BadRequest(message) => message,
        other => format!("{other:?}"),
    };
    let snapshot = object(value, "snapshot").map_err(words)?;
    only(
        snapshot,
        "snapshot",
        &["release_id", "artist", "title", "year", "tracklists"],
    )
    .map_err(words)?;
    let field = |key: &str| required(snapshot, key).map_err(words);
    let release = Release {
        release_id: integer(field("release_id")?, "snapshot.release_id", 1).map_err(words)?,
        artist: filled(field("artist")?, "snapshot.artist").map_err(words)?,
        title: filled(field("title")?, "snapshot.title").map_err(words)?,
        year: match field("year")? {
            Value::Null => None,
            year => Some(
                i32::try_from(integer(year, "snapshot.year", 0).map_err(words)?)
                    .map_err(|_| "snapshot.year is out of range".to_string())?,
            ),
        },
    };
    let Value::Array(lists) = field("tracklists")? else {
        return Err("snapshot.tracklists must be an array".to_string());
    };
    if lists.is_empty() || lists.len() > MAX_TRACKLISTS {
        return Err(format!(
            "snapshot.tracklists must hold 1 to {MAX_TRACKLISTS} tracklists"
        ));
    }
    let mut tracklists = Vec::with_capacity(lists.len());
    for (index, list) in lists.iter().enumerate() {
        let what = format!("snapshot.tracklists[{index}]");
        let Value::Array(tracks) = list else {
            return Err(format!("{what} must be an array"));
        };
        if tracks.is_empty() || tracks.len() > MAX_TRACKS {
            return Err(format!("{what} must hold 1 to {MAX_TRACKS} tracks"));
        }
        let mut positions = HashSet::new();
        let mut read = Vec::with_capacity(tracks.len());
        for (seq, track) in tracks.iter().enumerate() {
            let what = format!("{what}[{seq}]");
            let track = object(track, &what).map_err(words)?;
            only(track, &what, &["position", "title", "seconds"]).map_err(words)?;
            let field = |key: &str| required(track, key).map_err(words);
            let position =
                filled(field("position")?, &format!("{what}.position")).map_err(words)?;
            if !positions.insert(position.clone()) {
                return Err(format!("{what}.position {position:?} is in the list twice"));
            }
            let seconds = match field("seconds")? {
                Value::Null => None,
                seconds => Some(
                    u32::try_from(integer(seconds, &format!("{what}.seconds"), 0).map_err(words)?)
                        .map_err(|_| format!("{what}.seconds is out of range"))?,
                ),
            };
            read.push(Track {
                position,
                title: string(field("title")?, &format!("{what}.title")).map_err(words)?,
                seconds,
            });
        }
        tracklists.push(read);
    }
    Ok((release, tracklists))
}

fn upgrade(value: &Value) -> Result<Upgrade, Refusal> {
    let upgrade = object(value, "upgrade")?;
    only(upgrade, "upgrade", &["current_quality", "old_paths"])?;
    let old_paths = strings(
        required(upgrade, "old_paths")?,
        "upgrade.old_paths",
        MAX_OLD_PATHS,
    )?;
    if old_paths.is_empty() {
        return Err(bad("upgrade.old_paths must hold at least one path"));
    }
    Ok(Upgrade {
        current_quality: string(
            required(upgrade, "current_quality")?,
            "upgrade.current_quality",
        )?,
        old_paths,
    })
}

fn id(body: &Map<String, Value>) -> Result<i64, Refusal> {
    integer(required(body, "id")?, "id", 1)
}

fn policy(value: &Value, what: &str) -> Result<Policy, Refusal> {
    let name = string(value, what)?;
    Policy::parse(&name).ok_or_else(|| bad(&format!("{what} can't be {name:?}")))
}

/// Refuses a field of `map` not in `allowed`, so a misspelling fails.
fn only(map: &Map<String, Value>, what: &str, allowed: &[&str]) -> Result<(), Refusal> {
    match map.keys().find(|key| !allowed.contains(&key.as_str())) {
        Some(key) => Err(bad(&format!("unknown field `{key}` in {what}"))),
        None => Ok(()),
    }
}

fn required<'a>(map: &'a Map<String, Value>, key: &str) -> Result<&'a Value, Refusal> {
    map.get(key)
        .ok_or_else(|| bad(&format!("`{key}` is missing")))
}

fn object<'a>(value: &'a Value, what: &str) -> Result<&'a Map<String, Value>, Refusal> {
    value
        .as_object()
        .ok_or_else(|| bad(&format!("{what} must be an object")))
}

/// A whole number of at least `least`.
fn integer(value: &Value, what: &str, least: i64) -> Result<i64, Refusal> {
    value
        .as_i64()
        .filter(|n| *n >= least)
        .ok_or_else(|| bad(&format!("{what} must be a whole number from {least}")))
}

/// A string of at most `MAX_STRING_CHARS` characters.
fn string(value: &Value, what: &str) -> Result<String, Refusal> {
    match value {
        Value::String(text) if text.chars().count() <= MAX_STRING_CHARS => Ok(text.clone()),
        Value::String(_) => Err(bad(&format!(
            "{what} is over {MAX_STRING_CHARS} characters"
        ))),
        _ => Err(bad(&format!("{what} must be a string"))),
    }
}

/// A string that isn't empty.
fn filled(value: &Value, what: &str) -> Result<String, Refusal> {
    let text = string(value, what)?;
    if text.is_empty() {
        return Err(bad(&format!("{what} is empty")));
    }
    Ok(text)
}

/// An array of at most `most` strings.
fn strings(value: &Value, what: &str, most: usize) -> Result<Vec<String>, Refusal> {
    let Value::Array(items) = value else {
        return Err(bad(&format!("{what} must be an array")));
    };
    if items.len() > most {
        return Err(bad(&format!("{what} holds more than {most}")));
    }
    items
        .iter()
        .enumerate()
        .map(|(index, item)| string(item, &format!("{what}[{index}]")))
        .collect()
}

/// The `failed` state's name, which the player's failure event takes too.
fn failed() -> &'static str {
    State::Failed(Stage::Tag).name()
}

fn bad(message: &str) -> Refusal {
    Refusal::BadRequest(message.to_string())
}

fn internal(error: impl std::fmt::Display) -> Refusal {
    Refusal::Internal(error.to_string())
}
