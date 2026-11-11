//! Reading one request: the request line, the headers and a `Content-Length` body, each
//! with a cap, all within one deadline.

use std::io::{self, BufRead, Read};
use std::net::TcpStream;
use std::time::Instant;

use crate::reply::Refusal;
use crate::status;

/// The method every route takes but the status page.
pub(crate) const METHOD: &str = "POST";

/// The status page's method, for its path alone (ADR 0021).
const PAGE_METHOD: &str = "GET";

/// The longest request line or header line, in bytes.
const MAX_LINE: usize = 8 * 1024;
/// The most header lines.
const MAX_HEADERS: usize = 64;

/// A request read to its end.
#[derive(Debug)]
pub(crate) struct Request {
    /// `GET /status` rather than a POST: the browser's page, with no body.
    pub(crate) page: bool,
    pub(crate) path: String,
    /// Names are lowercase.
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
}

impl Request {
    /// The `Authorization` header, where both sign-ins are.
    pub(crate) fn authorization(&self) -> Option<&str> {
        authorization(&self.headers)
    }
}

/// The `Authorization` header among `headers`, whose names are lowercase.
fn authorization(headers: &[(String, String)]) -> Option<&str> {
    headers
        .iter()
        .find(|(n, _)| n == "authorization")
        .map(|(_, v)| v.as_str())
}

/// Why no request came of a connection.
#[derive(Debug)]
pub(crate) enum ReadError {
    /// The client closed or reset the connection first.
    Closed,
    /// A request to refuse; `unread` when the client may still be sending.
    Refused { refusal: Refusal, unread: bool },
}

impl From<Refusal> for ReadError {
    fn from(refusal: Refusal) -> Self {
        Self::Refused {
            refusal,
            unread: true,
        }
    }
}

impl From<io::Error> for ReadError {
    fn from(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => Self::Refused {
                refusal: Refusal::Timeout,
                unread: false,
            },
            _ => Self::Closed,
        }
    }
}

/// A stream whose reads fail with `TimedOut` once `until` has passed, so a client can't
/// stretch a request out by sending a byte at a time.
pub(crate) struct Deadline<'a> {
    stream: &'a TcpStream,
    until: Instant,
}

impl<'a> Deadline<'a> {
    pub(crate) fn new(stream: &'a TcpStream, until: Instant) -> Self {
        Self { stream, until }
    }
}

impl Read for Deadline<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        self.stream.set_read_timeout(Some(left))?;
        self.stream.read(buf)
    }
}

/// Reads a POST request, or a `GET /status` with no body. `max_body` gives a POST's cap on
/// its body in bytes from its path and `Authorization` header, read before the body is, or
/// the refusal that answers it unread.
pub(crate) fn read(
    reader: &mut impl BufRead,
    max_body: impl Fn(&str, Option<&str>) -> Result<usize, Refusal>,
) -> Result<Request, ReadError> {
    let request_line = line(reader, "the request line")?;
    let mut parts = request_line.split(' ');
    let (Some(method), Some(path), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(bad("the request line isn't `METHOD /path HTTP/1.1`"));
    };
    if !matches!(version, "HTTP/1.1" | "HTTP/1.0") {
        return Err(bad("only HTTP/1.1 and HTTP/1.0 are read"));
    }
    let (method, path) = (method.to_string(), path.to_string());
    let mut headers = Vec::new();
    loop {
        let header = line(reader, "a header")?;
        if header.is_empty() {
            break;
        }
        if headers.len() == MAX_HEADERS {
            return Err(Refusal::TooLarge(format!("more than {MAX_HEADERS} headers")).into());
        }
        let Some((name, value)) = header.split_once(':') else {
            return Err(bad("a header without a colon"));
        };
        if name.is_empty() || name.contains(char::is_whitespace) {
            return Err(bad("a header name with a space in it, or none"));
        }
        headers.push((name.to_ascii_lowercase(), value.trim().to_string()));
    }
    // The path is matched whole, as the routes match theirs: a query string makes another
    // path, which takes POST.
    let page = method == PAGE_METHOD && path == status::PATH;
    if method != METHOD && !page {
        let allow = if path == status::PATH {
            PAGE_METHOD
        } else {
            METHOD
        };
        return Err(Refusal::MethodNotAllowed(allow).into());
    }
    if page {
        // A length of zero says what a GET already is, and RFC 9110 allows it; any other
        // length, or a transfer coding, says there is a body.
        if headers.iter().any(|(name, value)| match name.as_str() {
            "content-length" => !is_zero(value),
            "transfer-encoding" => true,
            _ => false,
        }) {
            return Err(bad("a GET has no body"));
        }
        return Ok(Request {
            page,
            path,
            headers,
            body: Vec::new(),
        });
    }
    if headers.iter().any(|(name, _)| name == "transfer-encoding") {
        return Err(bad(
            "send the body with Content-Length, not Transfer-Encoding",
        ));
    }
    let mut lengths = headers.iter().filter(|(name, _)| name == "content-length");
    let (Some((_, text)), None) = (lengths.next(), lengths.next()) else {
        return Err(bad("a POST needs one Content-Length"));
    };
    // Digits only: `parse` would also take a leading `+`.
    let length = match text.parse::<usize>() {
        Ok(length) if text.bytes().all(|b| b.is_ascii_digit()) => length,
        _ => return Err(bad("Content-Length isn't a whole number")),
    };
    let max_body = max_body(&path, authorization(&headers))?;
    if length > max_body {
        return Err(Refusal::TooLarge(format!("the body is over {max_body} bytes")).into());
    }
    let mut body = vec![0; length];
    match reader.read_exact(&mut body) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
            return Err(ReadError::Closed);
        }
        Err(error) => return Err(error.into()),
    }
    Ok(Request {
        page,
        path,
        headers,
        body,
    })
}

/// Whether `text` is a `Content-Length` of zero: digits only, all zeros.
fn is_zero(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b == b'0')
}

fn bad(message: &str) -> ReadError {
    Refusal::BadRequest(message.to_string()).into()
}

/// One line without its `\r\n` (or bare `\n`), refused over `MAX_LINE` bytes.
fn line(reader: &mut impl BufRead, what: &str) -> Result<String, ReadError> {
    let mut buf = Vec::new();
    let cap = u64::try_from(MAX_LINE).unwrap_or(u64::MAX) + 1;
    reader.by_ref().take(cap).read_until(b'\n', &mut buf)?;
    if buf.pop() != Some(b'\n') {
        return Err(if buf.len() >= MAX_LINE {
            Refusal::TooLarge(format!("{what} is over {MAX_LINE} bytes")).into()
        } else {
            ReadError::Closed
        });
    }
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    String::from_utf8(buf).map_err(|_| bad(&format!("{what} isn't UTF-8")))
}
