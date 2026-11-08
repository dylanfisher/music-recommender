//! The slskd API client: searches (start, read, cancel, delete) and downloads (enqueue a
//! batch, list a user's transfers, cancel one), one request at a time. The base URL and the
//! API key are arguments; pacing and retries are the caller's.

use std::fmt::Write as _;
use std::time::Duration;

use serde::de::DeserializeOwned;
use ureq::Agent;
use ureq::http::{Response as HttpResponse, StatusCode};

use super::model;

/// A completed search with many peers is a few megabytes; this bounds what a misbehaving
/// server can send.
const MAX_REPLY_BYTES: u64 = 8 * 1024 * 1024;

/// For the whole request, connecting through reading the reply.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Every route is under this.
const API: &str = "/api/v0";

/// What went wrong with a request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// 401 or 403: the API key was refused.
    Refused(u16),
    /// 429: another request of the same kind is still being handled, a search being started
    /// or a batch being enqueued. Back off and try again.
    Busy,
    /// 404, with slskd's words: no such search or transfer, or the user is offline.
    NotFound(String),
    /// slskd's or the network's trouble: no reply, or a 5xx. Holds what was wrong.
    Trouble(String),
    /// A reply that isn't what slskd sends: over the size cap, not the JSON expected, or a
    /// status not listed above. Holds what was wrong.
    BadReply(String),
}

/// A search as slskd reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Search {
    /// slskd's id for it.
    pub id: String,
    /// Whether it has finished, by its timeout or otherwise.
    pub complete: bool,
    /// The peers' answers so far; empty from `start_search`.
    pub responses: Vec<Response>,
}

/// One peer's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    /// The Soulseek user.
    pub username: String,
    /// Whether the peer has an upload slot free now.
    pub has_free_upload_slot: bool,
    /// The peer's upload queue.
    pub queue_length: u64,
    /// Bytes a second, as the peer reports it.
    pub upload_speed: u64,
    /// The files the peer will send.
    pub files: Vec<File>,
    /// The files it shares but won't send to this user.
    pub locked_files: Vec<File>,
}

/// A shared file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    /// The peer's full path, `\`-separated as most peers send it.
    pub filename: String,
    /// Bytes.
    pub size: u64,
    /// As the peer reported it, without the dot; empty when it reported none.
    pub extension: String,
    /// Seconds.
    pub length: Option<u32>,
    /// kbps.
    pub bit_rate: Option<u32>,
    /// Bits a sample.
    pub bit_depth: Option<u32>,
    /// Hz.
    pub sample_rate: Option<u32>,
    /// Whether the bitrate is variable.
    pub variable_bit_rate: Option<bool>,
    /// The peer won't send it: it came in the response's `lockedFiles`. slskd's own `isLocked`
    /// is always false, so it isn't read.
    pub locked: bool,
}

/// What enqueueing a batch came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Queued {
    /// slskd's id for the batch.
    pub batch_id: String,
    /// The files slskd didn't queue, each with its message. Empty when all were.
    pub failures: Vec<(String, String)>,
}

/// A transfer's state, from slskd's `TransferStates` flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferState {
    /// Asked of the peer.
    Requested,
    /// Queued, neither side named.
    Queued,
    /// Queued in slskd.
    QueuedLocally,
    /// Queued by the peer, which may hold it for days.
    QueuedRemotely,
    /// Connecting.
    Initializing,
    /// Bytes are arriving.
    InProgress,
    /// Complete.
    Succeeded,
    /// Cancelled.
    Cancelled,
    /// Timed out.
    TimedOut,
    /// Failed.
    Errored,
    /// The peer refused it.
    Rejected,
    /// Aborted.
    Aborted,
}

impl TransferState {
    /// The state slskd writes as `text`, its flags in any order.
    fn parse(text: &str) -> Option<Self> {
        let mut flags: Vec<&str> = text.split(',').map(str::trim).collect();
        flags.sort_unstable();
        let state = match flags.as_slice() {
            ["Requested"] => Self::Requested,
            ["Queued"] => Self::Queued,
            ["Locally", "Queued"] => Self::QueuedLocally,
            ["Queued", "Remotely"] => Self::QueuedRemotely,
            ["Initializing"] => Self::Initializing,
            ["InProgress"] => Self::InProgress,
            ["Completed", "Succeeded"] => Self::Succeeded,
            ["Cancelled", "Completed"] => Self::Cancelled,
            ["Completed", "TimedOut"] => Self::TimedOut,
            ["Completed", "Errored"] => Self::Errored,
            ["Completed", "Rejected"] => Self::Rejected,
            ["Aborted", "Completed"] => Self::Aborted,
            _ => return None,
        };
        Some(state)
    }
}

/// A download.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transfer {
    /// slskd's id for it.
    pub id: String,
    /// The peer's full path.
    pub filename: String,
    /// Bytes.
    pub size: u64,
    /// Where it is.
    pub state: TransferState,
    /// Bytes received.
    pub bytes_transferred: u64,
    /// Why it failed, when it did.
    pub exception: Option<String>,
}

/// A client for one slskd and key. It has no `Debug`, since it holds the key.
pub struct Client {
    agent: Agent,
    base_url: String,
    api_key: String,
}

impl Client {
    /// A client sending `api_key` to the slskd at `base_url`.
    #[must_use]
    pub fn new(base_url: &str, api_key: &str) -> Self {
        let agent = Agent::config_builder()
            .http_status_as_error(false)
            // A redirect would drop the key's header and come back as a misleading 401.
            .max_redirects(0)
            .timeout_global(Some(TIMEOUT))
            .build()
            .into();
        Self {
            agent,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
        }
    }

    /// Starts a search for `text` that slskd ends after `timeout`, sent in milliseconds.
    ///
    /// # Errors
    ///
    /// See [`Error`]; `Busy` means another search is still being started.
    pub fn start_search(&self, text: &str, timeout: Duration) -> Result<Search, Error> {
        let body = to_json(&model::SearchRequest {
            search_text: text,
            search_timeout: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
        });
        let reply = self.send(self.agent.post(self.url("/searches")), &body);
        parse::<model::Search>(reply).map(search)
    }

    /// The search with its responses so far.
    ///
    /// # Errors
    ///
    /// See [`Error`].
    pub fn search(&self, id: &str) -> Result<Search, Error> {
        let url = self.url(&format!("/searches/{}?includeResponses=true", segment(id)));
        parse::<model::Search>(self.call(self.agent.get(url))).map(search)
    }

    /// The search's responses alone.
    ///
    /// # Errors
    ///
    /// See [`Error`].
    pub fn responses(&self, id: &str) -> Result<Vec<Response>, Error> {
        let url = self.url(&format!("/searches/{}/responses", segment(id)));
        parse::<Vec<model::Response>>(self.call(self.agent.get(url)))
            .map(|responses| responses.into_iter().map(response).collect())
    }

    /// Stops a search that is running; its responses stay until it is deleted.
    ///
    /// # Errors
    ///
    /// See [`Error`].
    pub fn cancel_search(&self, id: &str) -> Result<(), Error> {
        let url = self.url(&format!("/searches/{}", segment(id)));
        self.send(self.agent.put(url), "").map(drop)
    }

    /// Deletes a search and its responses.
    ///
    /// # Errors
    ///
    /// See [`Error`].
    pub fn delete_search(&self, id: &str) -> Result<(), Error> {
        let url = self.url(&format!("/searches/{}", segment(id)));
        self.call(self.agent.delete(url)).map(drop)
    }

    /// Enqueues `files`, each a peer's full path and its size, from `username`, into the
    /// folder `destination` under slskd's downloads directory.
    ///
    /// # Errors
    ///
    /// See [`Error`]; `NotFound` holds slskd's words for an offline user.
    pub fn enqueue(
        &self,
        username: &str,
        files: &[(String, u64)],
        destination: &str,
    ) -> Result<Queued, Error> {
        let body = to_json(&model::BatchRequest {
            username,
            files: files
                .iter()
                .map(|(filename, size)| model::FileRequest {
                    filename,
                    size: *size,
                })
                .collect(),
            options: model::BatchOptions { destination },
        });
        let reply = self.send(
            self.agent.post(self.url("/transfers/downloads/batches")),
            &body,
        );
        parse::<model::BatchReply>(reply).map(|reply| Queued {
            batch_id: reply.batch.id,
            failures: reply
                .failures
                .into_iter()
                .map(|failure| (failure.filename, failure.message))
                .collect(),
        })
    }

    /// Every download from `username` slskd still has a record of, in its order.
    ///
    /// # Errors
    ///
    /// See [`Error`]; a state slskd sends that isn't a [`TransferState`] is a `BadReply`
    /// naming it.
    pub fn downloads(&self, username: &str) -> Result<Vec<Transfer>, Error> {
        let url = self.url(&format!("/transfers/downloads/{}", segment(username)));
        let user = parse::<model::User>(self.call(self.agent.get(url)))?;
        user.directories
            .into_iter()
            .flat_map(|directory| directory.files)
            .map(|transfer| {
                let state = TransferState::parse(&transfer.state).ok_or_else(|| {
                    Error::BadReply(format!("unknown transfer state {:?}", transfer.state))
                })?;
                Ok(Transfer {
                    id: transfer.id,
                    filename: transfer.filename,
                    size: transfer.size,
                    state,
                    bytes_transferred: transfer.bytes_transferred,
                    exception: transfer.exception,
                })
            })
            .collect()
    }

    /// Cancels a download and removes slskd's record of it.
    ///
    /// # Errors
    ///
    /// See [`Error`].
    pub fn cancel_download(&self, username: &str, id: &str) -> Result<(), Error> {
        let url = self.url(&format!(
            "/transfers/downloads/{}/{}?remove=true",
            segment(username),
            segment(id)
        ));
        self.call(self.agent.delete(url)).map(drop)
    }

    fn url(&self, path: &str) -> String {
        format!("{}{API}{path}", self.base_url)
    }

    /// Sends a request without a body and reads the reply.
    fn call(&self, request: ureq::RequestBuilder<ureq::typestate::WithoutBody>) -> Reply {
        reply(request.header("X-API-Key", &self.api_key).call())
    }

    /// Sends a request with a JSON body and reads the reply.
    fn send(&self, request: ureq::RequestBuilder<ureq::typestate::WithBody>, body: &str) -> Reply {
        reply(
            request
                .header("X-API-Key", &self.api_key)
                .header("Content-Type", "application/json")
                .send(body),
        )
    }
}

/// A reply's body, or what was wrong with it.
type Reply = Result<String, Error>;

/// The body of a 2xx reply, read up to the cap, or the error its status means.
fn reply(result: Result<HttpResponse<ureq::Body>, ureq::Error>) -> Reply {
    let mut response =
        result.map_err(|error| Error::Trouble(format!("no reply from slskd: {error}")))?;
    let status = response.status();
    let body = match response
        .body_mut()
        .with_config()
        .limit(MAX_REPLY_BYTES)
        .read_to_string()
    {
        Ok(body) => body,
        Err(ureq::Error::BodyExceedsLimit(_)) => {
            return Err(Error::BadReply(format!(
                "slskd's reply is over {MAX_REPLY_BYTES} bytes"
            )));
        }
        Err(error) => return Err(Error::Trouble(format!("no reply from slskd: {error}"))),
    };
    match status {
        _ if status.is_success() => Ok(body),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(Error::Refused(status.as_u16())),
        StatusCode::TOO_MANY_REQUESTS => Err(Error::Busy),
        StatusCode::NOT_FOUND => Err(Error::NotFound(not_found_words(&body))),
        _ if status.is_server_error() => Err(Error::Trouble(format!(
            "slskd answered {}",
            status.as_u16()
        ))),
        _ => Err(Error::BadReply(format!(
            "slskd answered an unexpected {}",
            status.as_u16()
        ))),
    }
}

/// slskd's words in a 404: none for a missing search or transfer, a JSON string for an
/// offline user, since its controllers send JSON. Plain text is read as it is, because the
/// string's encoding hasn't been seen from a running slskd yet.
fn not_found_words(body: &str) -> String {
    let body = body.trim();
    serde_json::from_str::<String>(body).unwrap_or_else(|_| body.to_string())
}

fn parse<T: DeserializeOwned>(reply: Reply) -> Result<T, Error> {
    serde_json::from_str(&reply?).map_err(|error| Error::BadReply(format!("slskd JSON: {error}")))
}

fn to_json(value: &impl serde::Serialize) -> String {
    // The request shapes are plain strings and numbers, which always serialize.
    serde_json::to_string(value).expect("a request serializes")
}

/// `text` as one URL path segment: everything but RFC 3986's unreserved characters
/// percent-encoded, so a user name with `/`, `?` or a space stays one segment.
fn segment(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            write!(encoded, "%{byte:02X}").expect("writing to a String succeeds");
        }
    }
    encoded
}

fn search(search: model::Search) -> Search {
    Search {
        id: search.id,
        complete: search.is_complete,
        responses: search.responses.into_iter().map(response).collect(),
    }
}

fn response(response: model::Response) -> Response {
    Response {
        username: response.username,
        has_free_upload_slot: response.has_free_upload_slot,
        queue_length: response.queue_length,
        upload_speed: response.upload_speed,
        files: response.files.into_iter().map(|f| file(f, false)).collect(),
        locked_files: response
            .locked_files
            .into_iter()
            .map(|f| file(f, true))
            .collect(),
    }
}

fn file(file: model::File, locked: bool) -> File {
    File {
        filename: file.filename,
        size: file.size,
        extension: file.extension,
        length: file.length,
        bit_rate: file.bit_rate,
        bit_depth: file.bit_depth,
        sample_rate: file.sample_rate,
        variable_bit_rate: file.is_variable_bit_rate,
        locked,
    }
}

#[cfg(test)]
mod tests {
    use super::{TransferState, segment};

    #[test]
    fn flags_read_in_any_order() {
        assert_eq!(
            TransferState::parse("Remotely, Queued"),
            Some(TransferState::QueuedRemotely)
        );
        assert_eq!(TransferState::parse("Completed"), None);
    }

    #[test]
    fn a_segment_keeps_only_unreserved_characters() {
        assert_eq!(segment("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(segment("a b/c?é"), "a%20b%2Fc%3F%C3%A9");
    }
}
