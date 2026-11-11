//! The routes: the token check, then the path, then the route's body.

use config::Config;
use features::clock::Clock;
use serde_json::{Map, Value};

use crate::reply::{Refusal, Reply};
use crate::request::Request;
use crate::{community, downloads, profile, status, token};

/// The profile's route, the one that takes a body over `Limits::max_body`.
pub(crate) const PROFILE: &str = "/v1/profile";

/// The downloads routes' prefix.
const DOWNLOADS: &str = "/v1/downloads/";

/// The service's version, as `ping` reports it.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Answers a request read to its end. The token is checked before the path, so a client
/// without it can't learn which routes exist. `GET /status`, the one request that isn't a
/// POST, signs in its own way: a browser can't send the bearer.
pub(crate) fn answer(request: &Request, config: &Config, clock: &dyn Clock) -> Reply {
    if request.page {
        return status::answer(request, config, clock);
    }
    if !token::authorizes(request.authorization(), config.player_token.secret()) {
        return Refusal::Unauthorized.into();
    }
    let result = match request.path.as_str() {
        "/v1/ping" => ping(&request.body),
        PROFILE => profile::answer(&request.body, config),
        "/v1/community" => object(&request.body).and_then(|body| community::answer(&body, config)),
        path => match path.strip_prefix(DOWNLOADS) {
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
