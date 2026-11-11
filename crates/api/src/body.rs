//! Reading a JSON body's fields strictly, for every route: an unknown field, a missing one or
//! one of the wrong type is a 400 that names it (docs/api.md, HTTP).

use serde_json::{Map, Value};

use crate::reply::Refusal;

/// Refuses a field of `map` not in `allowed`, so a misspelling fails.
pub(crate) fn only(map: &Map<String, Value>, what: &str, allowed: &[&str]) -> Result<(), Refusal> {
    match map.keys().find(|key| !allowed.contains(&key.as_str())) {
        Some(key) => Err(bad(&format!("unknown field `{key}` in {what}"))),
        None => Ok(()),
    }
}

pub(crate) fn required<'a>(map: &'a Map<String, Value>, key: &str) -> Result<&'a Value, Refusal> {
    map.get(key)
        .ok_or_else(|| bad(&format!("`{key}` is missing")))
}

pub(crate) fn object<'a>(value: &'a Value, what: &str) -> Result<&'a Map<String, Value>, Refusal> {
    value
        .as_object()
        .ok_or_else(|| bad(&format!("{what} must be an object")))
}

/// A whole number of at least `least`.
pub(crate) fn integer(value: &Value, what: &str, least: i64) -> Result<i64, Refusal> {
    value
        .as_i64()
        .filter(|n| *n >= least)
        .ok_or_else(|| bad(&format!("{what} must be a whole number from {least}")))
}

pub(crate) fn bad(message: &str) -> Refusal {
    Refusal::BadRequest(message.to_string())
}

pub(crate) fn internal(error: impl std::fmt::Display) -> Refusal {
    Refusal::Internal(error.to_string())
}
