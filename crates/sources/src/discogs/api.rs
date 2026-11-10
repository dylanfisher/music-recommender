//! The Discogs API client: `GET /releases/{id}` for a release's reply and the values scoring
//! reads from it, one request at a time. Pacing is the caller's, with [`super::pacing`].

use std::time::Duration;

use serde::Deserialize;
use ureq::Agent;
use ureq::http::{HeaderMap, StatusCode};

/// The API's own base URL. Tests pass a stub server's instead.
pub const BASE_URL: &str = "https://api.discogs.com";

/// Discogs requires a `User-Agent` naming the client.
const USER_AGENT: &str = concat!("music-recommender/", env!("CARGO_PKG_VERSION"));

/// A release's reply is a few kilobytes; this bounds what a misbehaving server can send.
const MAX_REPLY_BYTES: u64 = 1024 * 1024;

/// For the whole request, connecting through reading the reply.
const TIMEOUT: Duration = Duration::from_secs(30);

/// The values scoring reads from a release's reply, under the API's names: its `community`
/// values and the marketplace's two.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Community {
    /// `community.rating.average`.
    pub rating_average: f64,
    /// `community.rating.count`.
    pub rating_count: u64,
    /// `community.have`.
    pub have: u64,
    /// `community.want`.
    pub want: u64,
    /// `num_for_sale`, `None` when the reply has none.
    pub num_for_sale: Option<u64>,
    /// `lowest_price`, `None` when the reply has none or it is null.
    pub lowest_price: Option<f64>,
}

/// The rate-limit headers of a reply, each `None` when absent or not a number.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RateLimit {
    /// `X-Discogs-Ratelimit`: requests allowed in the moving 60-second window.
    pub limit: Option<u32>,
    /// `X-Discogs-Ratelimit-Remaining`.
    pub remaining: Option<u32>,
    /// `X-Discogs-Ratelimit-Used`: requests in the window from every client Discogs counts
    /// together, ours included.
    pub used: Option<u32>,
}

/// What a request for a release came to.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// 200: the body exactly as read, which the sync keeps whole (ADR 0019), and the values
    /// parsed from it.
    Found {
        /// The values parsed from `body`.
        values: Community,
        /// The reply's JSON as Discogs sent it.
        body: String,
    },
    /// 404: the release isn't on Discogs.
    NotFound,
    /// 401 or 403: the token was refused.
    Refused(u16),
    /// 429, with `Retry-After` in seconds if the server sent one.
    RateLimited(Option<Duration>),
    /// Discogs's or the network's trouble, whatever the release: no reply (the connection,
    /// DNS, TLS or the timeout failed), a 5xx, a 408 or a 425. Holds what was wrong.
    Trouble(String),
    /// A reply about the release: a 200 whose body isn't what the API sends (over the size
    /// cap, or JSON without the community values), or a status not listed above (a 400, a
    /// 410, a redirect, another 4xx, a 2xx that isn't 200). Holds what was wrong.
    ReleaseProblem(String),
}

/// A request's outcome and the reply's rate-limit headers.
#[derive(Clone, Debug, PartialEq)]
pub struct Reply {
    /// What the request came to.
    pub outcome: Outcome,
    /// Empty when there was no reply.
    pub rate_limit: RateLimit,
}

/// The release's JSON, keeping only what `Community` holds.
#[derive(Deserialize)]
struct Release {
    community: ReleaseCommunity,
    #[serde(default)]
    num_for_sale: Option<u64>,
    #[serde(default)]
    lowest_price: Option<f64>,
}

#[derive(Deserialize)]
struct ReleaseCommunity {
    have: u64,
    want: u64,
    rating: Rating,
}

#[derive(Deserialize)]
struct Rating {
    count: u64,
    average: f64,
}

/// A client for one token. It has no `Debug`, since it holds the token.
pub struct Client {
    agent: Agent,
    base_url: String,
    authorization: String,
}

impl Client {
    /// A client sending `token` to the API at `base_url` ([`BASE_URL`] outside tests).
    #[must_use]
    pub fn new(base_url: &str, token: &str) -> Self {
        let agent = Agent::config_builder()
            .http_status_as_error(false)
            // The release endpoint doesn't redirect, and a redirect would drop the
            // `Authorization` header and come back as a misleading 401.
            .max_redirects(0)
            .timeout_global(Some(TIMEOUT))
            .user_agent(USER_AGENT)
            .build()
            .into();
        Self {
            agent,
            base_url: base_url.trim_end_matches('/').to_string(),
            authorization: format!("Discogs token={token}"),
        }
    }

    /// Requests one release.
    #[must_use]
    pub fn release(&self, release_id: u64) -> Reply {
        let url = format!("{}/releases/{release_id}", self.base_url);
        let mut response = match self
            .agent
            .get(&url)
            .header("Authorization", &self.authorization)
            .call()
        {
            Ok(response) => response,
            Err(error) => {
                return Reply {
                    outcome: Outcome::Trouble(format!("network: {error}")),
                    rate_limit: RateLimit::default(),
                };
            }
        };
        let headers = response.headers();
        let rate_limit = RateLimit {
            limit: number(headers, "X-Discogs-Ratelimit"),
            remaining: number(headers, "X-Discogs-Ratelimit-Remaining"),
            used: number(headers, "X-Discogs-Ratelimit-Used"),
        };
        let retry_after = number(headers, "Retry-After").map(|s| Duration::from_secs(s.into()));
        let status = response.status();
        let outcome = match status {
            StatusCode::OK => match response
                .body_mut()
                .with_config()
                .limit(MAX_REPLY_BYTES)
                .read_to_string()
            {
                Ok(body) => parse(body),
                Err(ureq::Error::BodyExceedsLimit(_)) => {
                    Outcome::ReleaseProblem(format!("reply over {MAX_REPLY_BYTES} bytes"))
                }
                Err(error) => Outcome::Trouble(format!("network: {error}")),
            },
            StatusCode::NOT_FOUND => Outcome::NotFound,
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Outcome::Refused(status.as_u16()),
            StatusCode::TOO_MANY_REQUESTS => Outcome::RateLimited(retry_after),
            StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_EARLY => {
                Outcome::Trouble(format!("Discogs answered {}", status.as_u16()))
            }
            _ if status.is_server_error() => {
                Outcome::Trouble(format!("Discogs answered {}", status.as_u16()))
            }
            _ => Outcome::ReleaseProblem(format!("unexpected status {}", status.as_u16())),
        };
        Reply {
            outcome,
            rate_limit,
        }
    }
}

/// A header holding a whole number. One that's absent or malformed is `None`: pacing then
/// relies on its own window of send times, which is the guard ADR 0004 asks for anyway.
fn number(headers: &HeaderMap, name: &str) -> Option<u32> {
    headers.get(name)?.to_str().ok()?.trim().parse().ok()
}

fn parse(body: String) -> Outcome {
    match serde_json::from_str::<Release>(&body) {
        Ok(release) => Outcome::Found {
            values: Community {
                rating_average: release.community.rating.average,
                rating_count: release.community.rating.count,
                have: release.community.have,
                want: release.community.want,
                num_for_sale: release.num_for_sale,
                lowest_price: release.lowest_price,
            },
            body,
        },
        Err(error) => Outcome::ReleaseProblem(format!("release JSON: {error}")),
    }
}
