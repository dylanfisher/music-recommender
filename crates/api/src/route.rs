//! The routes: the token check, then the path, then the route's body.

use config::Config;
use serde_json::{Map, Value};

use crate::downloads;
use crate::reply::{Refusal, Reply};
use crate::request::Request;
use crate::token;

/// The downloads routes' prefix.
const DOWNLOADS: &str = "/v1/downloads/";

/// The service's version, as `ping` reports it.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Answers a request read to its end. The token is checked before the path, so a client
/// without it can't learn which routes exist.
pub(crate) fn answer(request: &Request, config: &Config) -> Reply {
    if !token::authorizes(
        request.header("authorization"),
        config.player_token.secret(),
    ) {
        return Refusal::Unauthorized.into();
    }
    let result = match request.path.as_str() {
        "/v1/ping" => ping(&request.body),
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
