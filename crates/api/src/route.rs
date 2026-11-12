//! The routes: the token check, then the path, then the route's body.

use config::Config;
use features::clock::Clock;
use features::list::SeedSlots;
use features::rescore::Requests;
use serde_json::{Map, Value};

use crate::reply::{Refusal, Reply};
use crate::request::Request;
use crate::{
    Limits, community, downloads, feedback, profile, recommendations, status, sync_status, token,
};

/// A route at a path of its own, outside the downloads prefix.
#[derive(Clone, Copy)]
enum Route {
    Ping,
    Profile,
    Community,
    Feedback(feedback::Route),
    SyncStatus,
    Recommendations,
}

/// Every route outside the downloads prefix, by its path. `max_body` caps a body by this
/// table before it is read, and `answer` dispatches by it.
const ROUTES: [(&str, Route); 8] = [
    ("/v1/ping", Route::Ping),
    ("/v1/profile", Route::Profile),
    ("/v1/community", Route::Community),
    ("/v1/feedback", Route::Feedback(feedback::Route::Set)),
    ("/v1/feedback/undo", Route::Feedback(feedback::Route::Undo)),
    ("/v1/feedback/list", Route::Feedback(feedback::Route::List)),
    ("/v1/sync/status", Route::SyncStatus),
    ("/v1/recommendations", Route::Recommendations),
];

impl Route {
    fn at(path: &str) -> Option<Self> {
        ROUTES
            .iter()
            .find(|(route, _)| *route == path)
            .map(|&(_, route)| route)
    }
}

/// The cap on the body of a POST to `path` with `authorization`, read before the body. The
/// profile is the one body larger than `Limits::max_body`, and only a client with the token
/// gets its cap, so no one else can make a thread hold 32 MB: a request to its path without
/// the token is refused before its body is read.
pub(crate) fn max_body(
    path: &str,
    authorization: Option<&str>,
    config: &Config,
    limits: &Limits,
) -> Result<usize, Refusal> {
    match Route::at(path) {
        Some(Route::Profile) => {
            if token::authorizes(authorization, config.player_token.secret()) {
                Ok(limits.max_profile_body)
            } else {
                Err(Refusal::Unauthorized)
            }
        }
        _ => Ok(limits.max_body),
    }
}

/// The downloads routes' prefix.
const DOWNLOADS: &str = "/v1/downloads/";

/// The service's version, as `ping` reports it.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Answers a request read to its end. The token is checked before the path, so a client
/// without it can't learn which routes exist. `GET /status`, the one request that isn't a
/// POST, signs in its own way: a browser can't send the bearer. A profile kept asks
/// `rescore` for the scores' rebuild.
pub(crate) fn answer(
    request: &Request,
    config: &Config,
    clock: &dyn Clock,
    rescore: &Requests,
    slots: &SeedSlots,
) -> Reply {
    if request.page {
        return status::answer(config, clock);
    }
    if !token::authorizes(request.authorization(), config.player_token.secret()) {
        return Refusal::Unauthorized.into();
    }
    let path = request.path.as_str();
    let result = match Route::at(path) {
        Some(Route::Ping) => ping(&request.body),
        Some(Route::Profile) => profile::answer(&request.body, config, rescore),
        Some(Route::Community) => {
            object(&request.body).and_then(|body| community::answer(&body, config))
        }
        Some(Route::Feedback(route)) => {
            object(&request.body).and_then(|body| feedback::answer(route, &body, config, clock))
        }
        Some(Route::SyncStatus) => {
            object(&request.body).and_then(|body| sync_status::answer(&body, config, clock))
        }
        Some(Route::Recommendations) => object(&request.body)
            .and_then(|body| recommendations::answer(&body, config, clock, slots)),
        None => match path.strip_prefix(DOWNLOADS) {
            Some(route) if downloads::ROUTES.contains(&route) => {
                object(&request.body).and_then(|body| downloads::answer(route, &body, config))
            }
            _ => Err(Refusal::NotFound),
        },
    };
    result.unwrap_or_else(Reply::from)
}

/// `POST /v1/ping`, body `{}`: `{"ok": true, "version": "…"}`.
fn ping(body: &[u8]) -> Result<Reply, Refusal> {
    let fields = object(body)?;
    if let Some(field) = fields.keys().next() {
        return Err(Refusal::BadRequest(format!("unknown field `{field}`")));
    }
    let mut reply = Map::new();
    reply.insert("version".to_string(), Value::String(VERSION.to_string()));
    Ok(Reply::ok(reply))
}

/// The body as a JSON object.
fn object(body: &[u8]) -> Result<Map<String, Value>, Refusal> {
    match serde_json::from_slice(body) {
        Ok(Value::Object(fields)) => Ok(fields),
        Ok(_) => Err(Refusal::BadRequest(
            "the body must be a JSON object".to_string(),
        )),
        Err(error) => Err(Refusal::BadRequest(format!("the body isn't JSON: {error}"))),
    }
}
