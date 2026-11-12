//! The community sync: drains the community database's queue through the Discogs API, one
//! request at a time, committing each reply before the next request (ADR 0004). The same
//! loop runs in a terminal (`sync`) and in the launch agent (`serve`).

use std::fmt;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use catalog::community::{Community, Failure, LockError, Request, RequestKind, Values};
use catalog::error::DatabaseError;
use config::Config;
use sources::discogs::api::{Client, Outcome, RateLimit, Reply};
use sources::discogs::pacing::{OURS_SPAN, Pacer};

use crate::clock::{Clock, unix_seconds};

/// How long the sync keeps its `send` rows, and so the span the status counts requests over.
pub(crate) const REQUESTS_SPAN: Duration = Duration::from_hours(1);

// The pacer reads its sends from the last `OURS_SPAN`, so they must be kept that long.
const _: () = assert!(REQUESTS_SPAN.as_secs() >= OURS_SPAN.as_secs());

/// How often `serve`, with nothing to do, looks in the database for work: the pause flag
/// cleared or a release due. It holds no lock while it looks.
const SERVE_POLL: Duration = Duration::from_secs(15);

/// How long `serve` waits after a run failed for a reason polling won't fix: the token
/// was refused or the database failed.
const SERVE_RETRY: Duration = Duration::from_mins(10);

/// This many replies without a fetch in between that are Discogs's or the network's
/// trouble (no reply, a 5xx, a 408, a 425) stop the run: sending the rest of the queue
/// would only wait and fail. The count is for one run, so `serve` tries again after its
/// wait. A 429 neither counts nor resets it.
const TROUBLE_LIMIT: u32 = 10;

/// This many replies without a fetch or a 404 in between that are about their release (a
/// body or a status the API doesn't send) stop the run: a client bug would otherwise fail
/// the whole queue.
const RELEASE_PROBLEMS_LIMIT: u32 = 50;

/// The longest the loop sleeps before it reads the pause flag again, so `sync pause`
/// takes effect within this time even during a backoff or a long `Retry-After`.
const PAUSE_CHECK_EVERY: Duration = Duration::from_secs(5);

/// What the log says when the sync stops yielding to another client sharing the Discogs rate
/// limit and sends normally again (ADR 0020). `yielding_message` says when it starts.
const SENDING_AGAIN_MESSAGE: &str = "sending again: no other client in the Discogs rate limit";

/// The line logged when the sync starts yielding. It names the pacer's `OURS_SPAN`, the time between
/// requests while yielding.
fn yielding_message() -> String {
    format!(
        "yielding: another client is using the Discogs rate limit, likely the player's \
         tagger; one request every {} s until it stops",
        OURS_SPAN.as_secs()
    )
}

/// What `sync pause` and `sync resume` print.
const PAUSED_MESSAGE: &str = "paused; a running sync stops after its reply in flight";
const RESUMED_MESSAGE: &str = "resumed";

/// Why a run of the sync ended without an error.
#[derive(Debug, PartialEq, Eq)]
pub enum Stopped {
    /// The pause flag is set.
    Paused,
    /// Nothing is due.
    Done,
    /// The run sent the number of requests it was limited to.
    Limit,
}

/// How a run went.
#[derive(Debug, PartialEq, Eq)]
pub struct Summary {
    /// Requests sent, retries included.
    pub requests: u64,
    /// Why it ended.
    pub stopped: Stopped,
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let why = match self.stopped {
            Stopped::Paused => "paused (sync resume to continue)",
            Stopped::Done => "nothing left to fetch",
            Stopped::Limit => "reached the limit",
        };
        write!(f, "requests sent: {}; {why}", self.requests)
    }
}

/// Why the sync couldn't run or had to stop.
#[derive(Debug)]
pub enum SyncError {
    /// Another sync holds the lock file.
    Busy(PathBuf),
    /// The lock file can't be opened or locked.
    Lock(PathBuf, io::Error),
    /// The community database failed.
    Database(DatabaseError),
    /// Discogs answered 401 or 403. Holds the config file's path and the status.
    TokenRefused(PathBuf, u16),
    /// `TROUBLE_LIMIT` replies without a fetch in between that were Discogs's or the
    /// network's trouble. Holds the last.
    Trouble(String),
    /// `RELEASE_PROBLEMS_LIMIT` replies without a fetch in between that were about their
    /// release. Holds the last.
    ReleaseProblems(String),
}

impl fmt::Display for SyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy(path) => write!(
                f,
                "another sync is running (it holds {}); stop it or `sync pause`",
                path.display()
            ),
            Self::Lock(path, error) => write!(f, "{}: {error}", path.display()),
            Self::Database(error) => error.fmt(f),
            Self::TokenRefused(config, status) => write!(
                f,
                "Discogs refused the token (HTTP {status}); check discogs_token in {}",
                config.display()
            ),
            Self::Trouble(problem) => write!(
                f,
                "{TROUBLE_LIMIT} replies in a row without values, the last: {problem}; \
                 check the network and Discogs's status"
            ),
            Self::ReleaseProblems(problem) => write!(
                f,
                "Discogs refused {RELEASE_PROBLEMS_LIMIT} releases without a fetch in between, \
                 the last: {problem}; check the client"
            ),
        }
    }
}

impl std::error::Error for SyncError {}

impl From<DatabaseError> for SyncError {
    fn from(error: DatabaseError) -> Self {
        Self::Database(error)
    }
}

impl From<LockError> for SyncError {
    fn from(error: LockError) -> Self {
        match error {
            LockError::Busy(path) => Self::Busy(path),
            LockError::Io(path, error) => Self::Lock(path, error),
        }
    }
}

/// Runs the sync with `client` until it is paused, nothing is due, or it has sent `limit`
/// requests. `log` gets a line for each reply and each wait, and one each time the sync
/// starts or stops yielding to another client.
///
/// # Errors
///
/// When another sync is running, the database fails, Discogs refuses the token, or the
/// replies come to nothing useful (see `TROUBLE_LIMIT` and `RELEASE_PROBLEMS_LIMIT`).
pub fn run(
    config: &Config,
    client: &Client,
    clock: &dyn Clock,
    limit: Option<u64>,
    log: &mut dyn FnMut(&str),
) -> Result<Summary, SyncError> {
    let _lock = Community::lock(&config.data_dir)?;
    let mut run = Run {
        db: Community::open(&config.data_dir)?,
        config,
        log,
        trouble: 0,
        trouble_request: None,
        problems: 0,
    };
    // A run killed while yielding left its time set; this run's pacer starts out sending.
    run.db.set_yielding(None)?;
    // Requests the last runs sent are still in Discogs's window, and its `Used` counts them.
    let mut pacer = Pacer::default();
    let now = clock.now();
    let span_start = now.checked_sub(OURS_SPAN).unwrap_or(now);
    pacer.seed(now, run.db.recent_sends(span_start)?);
    let result = run.drain(client, clock, limit, &mut pacer);
    // Nothing yields once the run ends, whatever the pacer last said: a run that stopped on
    // an error can have left the database behind it.
    let cleared = run.db.set_yielding(None);
    // The run's own error is the one returned; a failed cleanup is returned only after a run
    // that succeeded, and is otherwise only logged.
    if let (Err(_), Err(cleanup)) = (&result, &cleared) {
        (run.log)(&format!("error: {cleanup}"));
    }
    let summary = result?;
    cleared?;
    Ok(summary)
}

/// One run's state: the database, where to log, and the streaks that stop it.
struct Run<'a> {
    db: Community,
    config: &'a Config,
    log: &'a mut dyn FnMut(&str),
    /// Replies of trouble since the last fetch, and the request they were all for, if one.
    trouble: u32,
    trouble_request: Option<Request>,
    /// Replies about their release since the last fetch.
    problems: u32,
}

impl Run<'_> {
    /// Sends requests paced by `pacer` until the run stops.
    fn drain(
        &mut self,
        client: &Client,
        clock: &dyn Clock,
        limit: Option<u64>,
        pacer: &mut Pacer,
    ) -> Result<Summary, SyncError> {
        let mut requests = 0;
        loop {
            let stopped = |stopped| Ok(Summary { requests, stopped });
            if self.db.paused()? {
                return stopped(Stopped::Paused);
            }
            if limit.is_some_and(|limit| requests >= limit) {
                return stopped(Stopped::Limit);
            }
            let Some(request) = self.db.next_due(unix_seconds(clock.now()))? else {
                return stopped(Stopped::Done);
            };
            // The request is picked before the wait and kept: during the wait only the
            // pause flag is read again, not the queue.
            let mut logged = false;
            loop {
                let wait = pacer.wait(clock.now());
                if wait.is_zero() {
                    break;
                }
                if !logged {
                    (self.log)(&format!("waiting {} s", wait.as_secs_f64().ceil()));
                    logged = true;
                }
                clock.sleep(wait.min(PAUSE_CHECK_EVERY));
                if self.db.paused()? {
                    return stopped(Stopped::Paused);
                }
            }
            // On disk before the request, so a run killed while it is in flight still
            // counts it as ours, as Discogs does. Kept for the status's requests count,
            // longer than the pacer reads them.
            let sent_at = clock.now();
            self.db.record_send(sent_at, REQUESTS_SPAN)?;
            pacer.sent(sent_at);
            requests += 1;
            let reply = send(client, request);
            let now = clock.now();
            let was_yielding = pacer.yielding();
            pacer.replied(&reply, now);
            // Before the reply is recorded, which can end the run, so the log and the
            // database follow the pacer.
            if pacer.yielding() != was_yielding {
                self.yielding(pacer.yielding(), unix_seconds(now))?;
            }
            self.record(request, reply.outcome, unix_seconds(now))?;
        }
    }

    /// Records and logs that the sync started yielding to another client, or stopped.
    fn yielding(&mut self, yielding: bool, now_seconds: i64) -> Result<(), SyncError> {
        if yielding {
            self.db.set_yielding(Some(now_seconds))?;
            (self.log)(&yielding_message());
        } else {
            self.db.set_yielding(None)?;
            (self.log)(SENDING_AGAIN_MESSAGE);
        }
        Ok(())
    }

    /// Records what a reply came to in the database and logs it.
    ///
    /// # Errors
    ///
    /// When the database fails, the token was refused, or a streak reached its limit.
    fn record(
        &mut self,
        request: Request,
        outcome: Outcome,
        now_seconds: i64,
    ) -> Result<(), SyncError> {
        match outcome {
            Outcome::Found { values, body } => {
                let values = Values {
                    rating_average: values.rating_average,
                    rating_count: values.rating_count,
                    have: values.have,
                    want: values.want,
                    num_for_sale: values.num_for_sale,
                    lowest_price: values.lowest_price,
                };
                self.db.record(request.id, values, &body, now_seconds)?;
                (self.log)(&format!(
                    "{request}: rating {} ({}), have {}, want {}",
                    values.rating_average, values.rating_count, values.have, values.want
                ));
                self.trouble = 0;
                self.problems = 0;
            }
            Outcome::NotFound => {
                self.db.record_gone(request, now_seconds)?;
                (self.log)(&format!("{request}: not on Discogs (404)"));
                self.trouble = 0;
                self.problems = 0;
            }
            Outcome::Refused(status) => {
                let error = SyncError::TokenRefused(self.config.path.clone(), status);
                return self.stop(error, now_seconds);
            }
            Outcome::RateLimited(_) => {
                let message =
                    self.db
                        .record_request_error(request, "rate-limited (429)", now_seconds)?;
                (self.log)(&message);
            }
            Outcome::Trouble(problem) => {
                let message = self
                    .db
                    .record_request_error(request, &problem, now_seconds)?;
                (self.log)(&message);
                self.trouble_request = if self.trouble == 0 || self.trouble_request == Some(request)
                {
                    Some(request)
                } else {
                    None
                };
                self.trouble += 1;
                if self.trouble >= TROUBLE_LIMIT {
                    // A request that always gets trouble would head the queue at every
                    // run: count it as a release problem, so it waits its turn.
                    if let Some(request) = self.trouble_request {
                        self.db.record_failure(request, &problem, now_seconds)?;
                    }
                    return self.stop(SyncError::Trouble(problem), now_seconds);
                }
            }
            Outcome::ReleaseProblem(problem) => {
                let (failure, message) = self.db.record_failure(request, &problem, now_seconds)?;
                (self.log)(&message);
                if failure == Failure::Failed {
                    (self.log)(&format!("{request}: given up on"));
                }
                self.trouble = 0;
                self.problems += 1;
                if self.problems >= RELEASE_PROBLEMS_LIMIT {
                    return self.stop(SyncError::ReleaseProblems(problem), now_seconds);
                }
            }
        }
        Ok(())
    }

    /// Records `error` as the last error, for `sync status`, and returns it.
    fn stop(&mut self, error: SyncError, now_seconds: i64) -> Result<(), SyncError> {
        self.db.record_error(&error.to_string(), now_seconds)?;
        Err(error)
    }
}

/// Sends `request`. This build sends releases only; a list request is answered as a problem
/// with its reply, so it is deferred and, at the tenth, failed rather than sent wrongly.
fn send(client: &Client, request: Request) -> Reply {
    match request.kind {
        RequestKind::Release => client.release(request.id),
        RequestKind::ArtistReleases | RequestKind::MasterVersions => Reply {
            outcome: Outcome::ReleaseProblem("this build sends no list requests".to_string()),
            rate_limit: RateLimit::default(),
        },
    }
}

/// The launch agent's loop: runs the sync, then waits for work and runs it again. After a
/// run that failed it waits 10 minutes first; when another sync holds the lock it waits
/// 15 s and tries again, logging that once until a run gets the lock. Never returns.
pub fn serve(config: &Config, client: &Client, clock: &dyn Clock, log: &mut dyn FnMut(&str)) -> ! {
    let mut busy = false;
    loop {
        match run(config, client, clock, None, log) {
            Ok(summary) => {
                busy = false;
                log(&summary.to_string());
            }
            Err(error @ SyncError::Busy(_)) => {
                if !busy {
                    log(&format!("error: {error}"));
                    busy = true;
                }
                clock.sleep(SERVE_POLL);
                continue;
            }
            Err(error) => {
                busy = false;
                log(&format!("error: {error}"));
                clock.sleep(SERVE_RETRY);
            }
        }
        if let Err(error) = wait_for_work(config, clock) {
            log(&format!("error: {error}"));
            clock.sleep(SERVE_RETRY);
        }
    }
}

/// Returns when the sync isn't paused and a release is due, looking every 15 s, so
/// `sync resume` and a new `profile load` start a run within that time.
///
/// # Errors
///
/// When the database fails.
fn wait_for_work(config: &Config, clock: &dyn Clock) -> Result<(), SyncError> {
    let db = Community::open(&config.data_dir)?;
    while db.paused()? || db.next_due(unix_seconds(clock.now()))?.is_none() {
        clock.sleep(SERVE_POLL);
    }
    Ok(())
}

/// Sets or clears the pause flag. A running sync stops after the reply in flight. Returns
/// what to tell the owner.
///
/// # Errors
///
/// When the database fails.
pub fn set_paused(config: &Config, paused: bool) -> Result<&'static str, SyncError> {
    Community::open(&config.data_dir)?.set_paused(paused)?;
    Ok(if paused {
        PAUSED_MESSAGE
    } else {
        RESUMED_MESSAGE
    })
}
