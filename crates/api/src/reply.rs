//! Replies: a status, a body (JSON, or the status page's HTML) and the headers every reply
//! carries.

use std::io::{self, Write};

use serde_json::{Map, Value, json};

use crate::request::METHOD;
use crate::status;

const JSON: &str = "application/json";
const HTML: &str = "text/html; charset=utf-8";

/// The header a 401 names its sign-in in.
pub(crate) const WWW_AUTHENTICATE: &str = "WWW-Authenticate";

/// Why a request is refused with a status other than 200. The codes are in docs/api.md.
#[derive(Debug)]
pub(crate) enum Refusal {
    /// The request isn't HTTP/1.x this server reads, or its body isn't the route's JSON.
    BadRequest(String),
    /// A request line, header or body over its cap.
    TooLarge(String),
    /// No `Authorization: Bearer` header with the player token.
    Unauthorized,
    /// No route at this path.
    NotFound,
    /// A method other than POST, but for `GET /status`.
    MethodNotAllowed,
    /// The whole request didn't arrive within the read deadline.
    Timeout,
    /// Every connection slot is taken.
    Busy,
    /// The service failed to answer: its database couldn't be read or written. Holds what
    /// went wrong.
    Internal(String),
}

/// A reply ready to write.
pub(crate) struct Reply {
    status: u16,
    content_type: &'static str,
    headers: Vec<(&'static str, &'static str)>,
    body: String,
}

impl Reply {
    /// An HTML page with `status` and `headers`, for a browser.
    pub(crate) fn html(
        status: u16,
        headers: Vec<(&'static str, &'static str)>,
        body: String,
    ) -> Self {
        Self {
            status,
            content_type: HTML,
            headers,
            body,
        }
    }

    /// `{"ok": true, ...fields}`, with a 200.
    pub(crate) fn ok(fields: Map<String, Value>) -> Self {
        let mut body = Map::new();
        body.insert("ok".to_string(), Value::Bool(true));
        body.extend(fields);
        Self {
            status: 200,
            content_type: JSON,
            headers: Vec::new(),
            body: Value::Object(body).to_string(),
        }
    }

    /// `{"ok": false, "error": {"code", "message", ...fields}}`, with a 200: a request
    /// understood and refused.
    pub(crate) fn refused(code: &str, message: &str, fields: Map<String, Value>) -> Self {
        let mut error = Map::new();
        error.insert("code".to_string(), Value::String(code.to_string()));
        error.insert("message".to_string(), Value::String(message.to_string()));
        error.extend(fields);
        Self {
            status: 200,
            content_type: JSON,
            headers: Vec::new(),
            body: json!({"ok": false, "error": error}).to_string(),
        }
    }

    /// Writes the status line, the headers and the body. `Content-Length` and
    /// `Connection: close` go on every reply: the player's client reads a sized body and
    /// opens a new connection for each request.
    pub(crate) fn write(&self, out: &mut impl Write) -> io::Result<()> {
        let mut head = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n\
             Connection: close\r\n",
            self.status,
            reason(self.status),
            self.content_type,
            self.body.len()
        );
        for (name, value) in &self.headers {
            head.push_str(name);
            head.push_str(": ");
            head.push_str(value);
            head.push_str("\r\n");
        }
        head.push_str("\r\n");
        out.write_all(head.as_bytes())?;
        out.write_all(self.body.as_bytes())
    }
}

impl From<Refusal> for Reply {
    fn from(refusal: Refusal) -> Self {
        let (status, code, message, headers) = match refusal {
            Refusal::BadRequest(message) => (400, "bad_request", message, vec![]),
            Refusal::TooLarge(message) => (400, "too_large", message, vec![]),
            Refusal::Unauthorized => (
                401,
                "unauthorized",
                "send the player token as `Authorization: Bearer <token>`".to_string(),
                vec![(WWW_AUTHENTICATE, "Bearer")],
            ),
            Refusal::NotFound => (404, "not_found", "no such route".to_string(), vec![]),
            Refusal::MethodNotAllowed => (
                405,
                "method_not_allowed",
                format!(
                    "every route takes {METHOD} but the page, GET {}",
                    status::PATH
                ),
                vec![("Allow", METHOD)],
            ),
            Refusal::Timeout => (
                408,
                "timeout",
                "the request didn't arrive in time".to_string(),
                vec![],
            ),
            Refusal::Busy => (
                503,
                "busy",
                "too many connections; try again".to_string(),
                vec![],
            ),
            Refusal::Internal(message) => (500, "internal", message, vec![]),
        };
        Self {
            status,
            content_type: JSON,
            headers,
            body: json!({"ok": false, "error": {"code": code, "message": message}}).to_string(),
        }
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Unknown",
    }
}
