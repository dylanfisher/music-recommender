//! Reading a JSON body's fields strictly, for every route: an unknown field, a missing one or
//! one of the wrong type is a 400 that names it (docs/api.md, HTTP).

use serde_json::{Map, Value};

use crate::reply::Refusal;

/// The longest string field, in characters, unless a field says otherwise.
const MAX_STRING_CHARS: usize = 1000;

/// What a refusal calls the body itself, as `only`'s `what`.
pub(crate) const BODY: &str = "the body";

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

/// A string of at most `MAX_STRING_CHARS` characters.
pub(crate) fn string(value: &Value, what: &str) -> Result<String, Refusal> {
    match value {
        Value::String(text) if text.chars().count() <= MAX_STRING_CHARS => Ok(text.clone()),
        Value::String(_) => Err(bad(&format!(
            "{what} is over {MAX_STRING_CHARS} characters"
        ))),
        _ => Err(bad(&format!("{what} must be a string"))),
    }
}

/// An array's items.
pub(crate) fn array<'a>(value: &'a Value, what: &str) -> Result<&'a [Value], Refusal> {
    value
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| bad(&format!("{what} must be an array")))
}

/// An array of at most `most` strings.
pub(crate) fn strings(value: &Value, what: &str, most: usize) -> Result<Vec<String>, Refusal> {
    let items = array(value, what)?;
    if items.len() > most {
        return Err(bad(&format!("{what} holds more than {most}")));
    }
    items
        .iter()
        .enumerate()
        .map(|(index, item)| string(item, &format!("{what}[{index}]")))
        .collect()
}

pub(crate) fn bad(message: &str) -> Refusal {
    Refusal::BadRequest(message.to_string())
}

pub(crate) fn internal(error: impl std::fmt::Display) -> Refusal {
    Refusal::Internal(error.to_string())
}
