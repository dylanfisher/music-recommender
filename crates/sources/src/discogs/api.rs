//! The Discogs API client: `GET /releases/{id}` for a release's reply and the values scoring
//! reads from it, and `GET /artists/{id}/releases` and `GET /masters/{id}/versions` for a page
//! of a list with each row's owners and wants, one request at a time. Pacing is the caller's,
//! with [`super::pacing`].

use std::time::Duration;

use serde::Deserialize;
use ureq::Agent;
use ureq::http::{HeaderMap, StatusCode};

/// The API's own base URL. Tests pass a stub server's instead.
pub const BASE_URL: &str = "https://api.discogs.com";

/// Discogs requires a `User-Agent` naming the client.
pub(crate) const USER_AGENT: &str = concat!("music-recommender/", env!("CARGO_PKG_VERSION"));

/// A release's reply is a few kilobytes and a list page of 100 rows about 60; this bounds
/// what a misbehaving server can send.
const MAX_REPLY_BYTES: u64 = 1024 * 1024;

/// The rows a list request asks for, the most the API sends a page.
const PER_PAGE: u32 = 100;

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
    /// `num_for_sale`, `None` when the reply has none or it isn't a whole number.
    pub num_for_sale: Option<u64>,
    /// `lowest_price`, `None` when the reply has none, it is null or it isn't a number.
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

/// One page of an artist's releases or a master's versions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page {
    /// `pagination.pages`: how many pages the list has.
    pub pages: u64,
    /// Its rows, in the reply's order.
    pub rows: Vec<ListRow>,
}

/// One row of a list page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListRow {
    /// Whether `id` is a master's: an artist's releases list its masters and its releases
    /// without one; a master's versions are releases.
    pub master: bool,
    /// The release's or master's id.
    pub id: u64,
    /// `stats.community.in_collection`.
    pub have: u64,
    /// `stats.community.in_wantlist`.
    pub want: u64,
}

/// What a request came to. `T` is what a 200 is read as: a release's [`Community`] values,
/// or a list's [`Page`].
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome<T = Community> {
    /// 200: the body exactly as read, which the sync keeps whole (ADR 0019), and the values
    /// parsed from it.
    Found {
        /// The values parsed from `body`.
        values: T,
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
pub struct Reply<T = Community> {
    /// What the request came to.
    pub outcome: Outcome<T>,
    /// Empty when there was no reply.
    pub rate_limit: RateLimit,
}

/// The release's JSON, keeping only what `Community` holds. The community values are
/// required and typed. The price fields are read as JSON and typed in `parse`, so one of an
/// unexpected type loses its own value and not the release's.
#[derive(Deserialize)]
struct Release {
    community: ReleaseCommunity,
    num_for_sale: Option<serde_json::Value>,
    lowest_price: Option<serde_json::Value>,
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

/// An artist's releases page, keeping what `Page` holds.
#[derive(Deserialize)]
struct ArtistReleases {
    pagination: Pagination,
    releases: Vec<ArtistRow>,
}

/// A master's versions page, keeping what `Page` holds.
#[derive(Deserialize)]
struct MasterVersions {
    pagination: Pagination,
    versions: Vec<VersionRow>,
}

#[derive(Deserialize)]
struct Pagination {
    pages: u64,
}

#[derive(Deserialize)]
struct ArtistRow {
    id: u64,
    #[serde(rename = "type")]
    kind: RowType,
    stats: Stats,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum RowType {
    Master,
    Release,
}

#[derive(Deserialize)]
struct VersionRow {
    id: u64,
    stats: Stats,
}

#[derive(Deserialize)]
struct Stats {
    community: RowCommunity,
}

#[derive(Deserialize)]
struct RowCommunity {
    in_collection: u64,
    in_wantlist: u64,
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
        self.get(&format!("/releases/{release_id}"), parse)
    }

    /// Requests page `page`, from 1, of an artist's releases, `PER_PAGE` rows a page.
    #[must_use]
    pub fn artist_releases(&self, artist_id: u64, page: u64) -> Reply<Page> {
        self.get(
            &format!("/artists/{artist_id}/releases?per_page={PER_PAGE}&page={page}"),
            parse_artist_releases,
        )
    }

    /// Requests page `page`, from 1, of a master's versions, `PER_PAGE` rows a page.
    #[must_use]
    pub fn master_versions(&self, master_id: u64, page: u64) -> Reply<Page> {
        self.get(
            &format!("/masters/{master_id}/versions?per_page={PER_PAGE}&page={page}"),
            parse_master_versions,
        )
    }

    /// Sends `GET path` and reads a 200's body with `parse`. Every kind of request shares the
    /// statuses' meanings, the size cap and the timeout.
    fn get<T>(&self, path: &str, parse: fn(String) -> Outcome<T>) -> Reply<T> {
        let url = format!("{}{path}", self.base_url);
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

/// A list page's rows, or what was wrong with them: a row without its counts is a problem
/// with the reply, as a release without its community values is.
fn parse_list<L: serde::de::DeserializeOwned>(
    body: String,
    what: &str,
    page: fn(L) -> Page,
) -> Outcome<Page> {
    match serde_json::from_str::<L>(&body) {
        Ok(list) => Outcome::Found {
            values: page(list),
            body,
        },
        Err(error) => Outcome::ReleaseProblem(format!("{what} JSON: {error}")),
    }
}

fn parse_artist_releases(body: String) -> Outcome<Page> {
    parse_list(body, "artist releases", |list: ArtistReleases| Page {
        pages: list.pagination.pages,
        rows: list
            .releases
            .into_iter()
            .map(|row| ListRow {
                master: matches!(row.kind, RowType::Master),
                id: row.id,
                have: row.stats.community.in_collection,
                want: row.stats.community.in_wantlist,
            })
            .collect(),
    })
}

fn parse_master_versions(body: String) -> Outcome<Page> {
    parse_list(body, "master versions", |list: MasterVersions| Page {
        pages: list.pagination.pages,
        rows: list
            .versions
            .into_iter()
            .map(|row| ListRow {
                master: false,
                id: row.id,
                have: row.stats.community.in_collection,
                want: row.stats.community.in_wantlist,
            })
            .collect(),
    })
}

fn parse(body: String) -> Outcome {
    match serde_json::from_str::<Release>(&body) {
        Ok(release) => Outcome::Found {
            values: Community {
                rating_average: release.community.rating.average,
                rating_count: release.community.rating.count,
                have: release.community.have,
                want: release.community.want,
                num_for_sale: release
                    .num_for_sale
                    .as_ref()
                    .and_then(serde_json::Value::as_u64),
                lowest_price: release
                    .lowest_price
                    .as_ref()
                    .and_then(serde_json::Value::as_f64),
            },
            body,
        },
        Err(error) => Outcome::ReleaseProblem(format!("release JSON: {error}")),
    }
}
