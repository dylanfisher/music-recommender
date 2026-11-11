//! The client for data.discogs.com: a year's listing and a file's download, one request at
//! a time, with no pacing of its own. The site rate-limits downloads (a 429 with
//! `Retry-After: 2750` after a few requests in a row), so a reply that asks for a wait is
//! [`Fetched::Wait`], and the caller waits it out on its clock and asks again. Redirects
//! aren't followed: the site sends none.

use std::io::{self, Write};
use std::time::Duration;

use sha2::{Digest, Sha256};
use ureq::Agent;
use ureq::http::header::{AsHeaderName, CONTENT_LENGTH, RETRY_AFTER};
use ureq::http::{HeaderMap, StatusCode};

use super::api::USER_AGENT;
use super::dump::hex;
use super::listing;

/// A listing page is about 10 KB; this bounds what a misbehaving server can send.
const MAX_LISTING_BYTES: u64 = 1024 * 1024;

/// For connecting and for the reply's head. A download has no limit on its whole length,
/// 10 GB from the NAS's side of the LAN, only on each read.
const TIMEOUT: Duration = Duration::from_secs(60);

/// What a request came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Fetched<T> {
    /// A 200, read whole.
    Done(T),
    /// A 429, or a 503 with a `Retry-After`: ask again after the wait it names in seconds,
    /// or, when it names none or a date, after the caller's own wait.
    Wait(Option<Duration>),
    /// Anything else: no reply, another status, a body cut short. In words.
    Failed(String),
}

/// A file downloaded to a writer.
#[derive(Debug, PartialEq, Eq)]
pub struct Downloaded {
    /// The reply's `Content-Length`, if it had one.
    pub content_length: Option<u64>,
    /// The bytes written.
    pub written: u64,
    /// Their SHA-256, as lowercase hex.
    pub sha256: String,
}

/// The client. It holds no token: the dumps are public.
pub struct Client {
    agent: Agent,
    base_url: String,
}

impl Client {
    /// A client for the site at `base_url` ([`listing::BASE_URL`] outside tests).
    #[must_use]
    pub fn new(base_url: &str) -> Self {
        let agent = Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_connect(Some(TIMEOUT))
            .timeout_recv_response(Some(TIMEOUT))
            .timeout_per_read(Some(TIMEOUT))
            .user_agent(USER_AGENT)
            .build()
            .into();
        Self {
            agent,
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    /// The listing page of `year`'s folder.
    #[must_use]
    pub fn listing(&self, year: i64) -> Fetched<String> {
        let mut response = match self.get(&listing::listing_path(year)) {
            Ok(response) => response,
            Err(other) => return other,
        };
        match response
            .body_mut()
            .with_config()
            .limit(MAX_LISTING_BYTES)
            .read_to_string()
        {
            Ok(page) => Fetched::Done(page),
            Err(error) => Fetched::Failed(format!("listing of {year}: {error}")),
        }
    }

    /// Downloads the set's file `name` into `to`, hashing it on the way.
    #[must_use]
    pub fn download(&self, name: &str, to: &mut dyn Write) -> Fetched<Downloaded> {
        let Some(path) = listing::download_path(name) else {
            return Fetched::Failed(format!("{name}: not a dump set's file name"));
        };
        let mut response = match self.get(&path) {
            Ok(response) => response,
            Err(other) => return other,
        };
        let content_length = header(response.headers(), CONTENT_LENGTH);
        let mut hashing = Hashing {
            to,
            hash: Sha256::new(),
        };
        let mut body = response.body_mut().with_config().limit(u64::MAX).reader();
        match io::copy(&mut body, &mut hashing) {
            Ok(written) => Fetched::Done(Downloaded {
                content_length,
                written,
                sha256: hex(hashing.hash),
            }),
            Err(error) => Fetched::Failed(format!("{name}: {error}")),
        }
    }

    /// Sends a GET for `path`, keeping a 200 and turning every other reply into its
    /// outcome.
    fn get<T>(&self, path: &str) -> Result<ureq::http::Response<ureq::Body>, Fetched<T>> {
        let url = format!("{}{path}", self.base_url);
        let response = self
            .agent
            .get(&url)
            .call()
            .map_err(|error| Fetched::Failed(format!("{url}: network: {error}")))?;
        let status = response.status();
        let asks_to_wait = response.headers().contains_key(RETRY_AFTER);
        let retry_after = header(response.headers(), RETRY_AFTER).map(Duration::from_secs);
        match status {
            StatusCode::OK => Ok(response),
            StatusCode::TOO_MANY_REQUESTS => Err(Fetched::Wait(retry_after)),
            StatusCode::SERVICE_UNAVAILABLE if asks_to_wait => Err(Fetched::Wait(retry_after)),
            _ => Err(Fetched::Failed(format!("{url}: HTTP {}", status.as_u16()))),
        }
    }
}

/// A header holding a whole number; absent or malformed is `None`. A `Retry-After` given
/// as a date is `None` too, and waited out as one without a value.
fn header(headers: &HeaderMap, name: impl AsHeaderName) -> Option<u64> {
    headers.get(name)?.to_str().ok()?.trim().parse().ok()
}

/// A writer that hashes what passes through it.
struct Hashing<'a> {
    to: &'a mut dyn Write,
    hash: Sha256,
}

impl Write for Hashing<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.to.write(buf)?;
        self.hash.update(&buf[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.to.flush()
    }
}
