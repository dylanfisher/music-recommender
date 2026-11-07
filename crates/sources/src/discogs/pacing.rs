//! Pacing for the Discogs API: how long to wait before the next request. It keeps its own
//! window of send times under the limit the replies report, waits when `Remaining` is 0,
//! waits out a 429, and backs off after server and network errors.
//!
//! It holds no clock and never sleeps: the caller passes the time in and does the waiting,
//! so tests drive it with made-up times.

use std::collections::VecDeque;
use std::time::{Duration, SystemTime};

use super::api::{Outcome, Reply};

/// Discogs counts requests in a moving window this long.
const WINDOW: Duration = Duration::from_secs(60);

/// The authenticated limit, used until a reply reports one.
const DEFAULT_LIMIT: u32 = 60;

/// A 429 without `Retry-After` waits this long.
const RATE_LIMITED_WAIT: Duration = Duration::from_secs(60);

/// The first wait after a server or network error, doubled for each one after it.
const FIRST_BACKOFF: Duration = Duration::from_secs(5);
const MAX_BACKOFF: Duration = Duration::from_mins(10);

/// The pacing state for one run of the sync.
#[derive(Debug)]
pub struct Pacer {
    /// Send times within the last `WINDOW`, oldest first.
    sent: VecDeque<SystemTime>,
    limit: u32,
    /// No request before this time: set by a 429, a backoff or `Remaining` reaching 0.
    not_before: Option<SystemTime>,
    /// The last backoff, doubled on the next error and cleared by a reply that counts.
    backoff: Option<Duration>,
}

impl Default for Pacer {
    fn default() -> Self {
        Self {
            sent: VecDeque::new(),
            limit: DEFAULT_LIMIT,
            not_before: None,
            backoff: None,
        }
    }
}

impl Pacer {
    /// How long to wait at `now` before sending the next request. Zero means send now.
    #[must_use]
    pub fn wait(&self, now: SystemTime) -> Duration {
        let mut until = self.not_before.unwrap_or(now);
        let in_window = self.in_window(now);
        let limit = self.limit as usize;
        if in_window.len() >= limit {
            // The send that has to leave the window before one more fits.
            until = until.max(in_window[in_window.len() - limit] + WINDOW);
        }
        since(now, until)
    }

    /// Records that a request was sent at `now`.
    pub fn sent(&mut self, now: SystemTime) {
        self.sent.push_back(now);
        while self.sent.front().is_some_and(|&t| since(t, now) >= WINDOW) {
            self.sent.pop_front();
        }
    }

    /// Records the reply to the last request, received at `now`.
    pub fn replied(&mut self, reply: &Reply, now: SystemTime) {
        if let Some(limit) = reply.rate_limit.limit.filter(|&limit| limit > 0) {
            self.limit = limit;
        }
        let wait = match &reply.outcome {
            Outcome::Found(_) | Outcome::NotFound | Outcome::Refused(_) => {
                self.backoff = None;
                None
            }
            Outcome::RateLimited(retry_after) => Some(retry_after.unwrap_or(RATE_LIMITED_WAIT)),
            Outcome::ServerError(_) | Outcome::Network(_) | Outcome::BadReply(_) => {
                let backoff = self
                    .backoff
                    .map_or(FIRST_BACKOFF, |last| (last * 2).min(MAX_BACKOFF));
                self.backoff = Some(backoff);
                Some(backoff)
            }
        };
        if let Some(wait) = wait {
            self.wait_until(now + wait);
        }
        if reply.rate_limit.remaining == Some(0) {
            // Discogs's window frees a request when the oldest one in it turns 60 s old.
            // With no send of ours in it, the requests were someone else's with this
            // token, so wait a whole window.
            let oldest = self.in_window(now).first().copied().unwrap_or(now);
            self.wait_until(oldest + WINDOW);
        }
    }

    fn wait_until(&mut self, until: SystemTime) {
        self.not_before = Some(self.not_before.map_or(until, |t| t.max(until)));
    }

    fn in_window(&self, now: SystemTime) -> Vec<SystemTime> {
        self.sent
            .iter()
            .copied()
            .filter(|&t| since(t, now) < WINDOW)
            .collect()
    }
}

/// The time from `earlier` to `later`, zero if `later` isn't later. The system clock can
/// step back (a time sync); pacing then waits by the later reading, which is the longer
/// wait, rather than failing the sync.
fn since(earlier: SystemTime, later: SystemTime) -> Duration {
    later.duration_since(earlier).unwrap_or(Duration::ZERO)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::Pacer;
    use crate::discogs::api::{Community, Outcome, RateLimit, Reply};

    const FOUND: Outcome = Outcome::Found(Community {
        rating_average: 4.0,
        rating_count: 1,
        have: 1,
        want: 1,
    });

    fn at(seconds: f64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs_f64(1_000_000.0 + seconds)
    }

    fn reply(outcome: Outcome, rate_limit: RateLimit) -> Reply {
        Reply {
            outcome,
            rate_limit,
        }
    }

    /// Sends `count` requests as fast as the pacer allows, each answered at once with
    /// `answer(n)`, and returns the send times in seconds from the start.
    fn run(pacer: &mut Pacer, count: usize, answer: impl Fn(usize) -> Reply) -> Vec<f64> {
        let mut now = at(0.0);
        let mut sends = Vec::new();
        for n in 0..count {
            now += pacer.wait(now);
            pacer.sent(now);
            sends.push(now.duration_since(at(0.0)).unwrap().as_secs_f64());
            // Each request takes 100 ms.
            now += Duration::from_millis(100);
            pacer.replied(&answer(n), now);
        }
        sends
    }

    fn most_in_any_minute(sends: &[f64]) -> usize {
        sends
            .iter()
            .map(|&start| {
                sends
                    .iter()
                    .filter(|&&t| t >= start && t < start + 60.0)
                    .count()
            })
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn never_sends_61_requests_in_a_minute() {
        let mut pacer = Pacer::default();
        let sends = run(&mut pacer, 200, |_| reply(FOUND, RateLimit::default()));
        assert_eq!(most_in_any_minute(&sends), 60);
        // The first 60 go out without waiting, the 61st when the first leaves the window.
        assert!(sends[59] < 6.0, "{}", sends[59]);
        assert!((sends[60] - 60.0).abs() < 1e-6, "{}", sends[60]);
    }

    #[test]
    fn keeps_under_the_limit_a_reply_reports() {
        let mut pacer = Pacer::default();
        let limit = RateLimit {
            limit: Some(25),
            used: None,
            remaining: None,
        };
        let sends = run(&mut pacer, 100, |_| reply(FOUND, limit));
        assert_eq!(most_in_any_minute(&sends), 25);
    }

    #[test]
    fn waits_when_remaining_reaches_zero() {
        let mut pacer = Pacer::default();
        let mut now = at(0.0);
        pacer.sent(now);
        now = at(1.0);
        let exhausted = RateLimit {
            limit: Some(60),
            used: Some(60),
            remaining: Some(0),
        };
        pacer.replied(&reply(FOUND, exhausted), now);
        // Our one send in the window leaves it at 60 s.
        assert_eq!(pacer.wait(now), Duration::from_secs(59));
    }

    #[test]
    fn a_429_waits_what_it_says_or_a_minute() {
        let mut pacer = Pacer::default();
        let now = at(0.0);
        pacer.sent(now);
        let limited = Outcome::RateLimited(Some(Duration::from_secs(17)));
        pacer.replied(&reply(limited, RateLimit::default()), now);
        assert_eq!(pacer.wait(now), Duration::from_secs(17));
        assert_eq!(pacer.wait(at(10.0)), Duration::from_secs(7));
        assert_eq!(pacer.wait(at(17.0)), Duration::ZERO);

        let mut pacer = Pacer::default();
        pacer.replied(
            &reply(Outcome::RateLimited(None), RateLimit::default()),
            now,
        );
        assert_eq!(pacer.wait(now), Duration::from_secs(60));
    }

    #[test]
    fn backoff_doubles_to_ten_minutes_and_resets_on_success() {
        let mut pacer = Pacer::default();
        let mut now = at(0.0);
        let mut waits = Vec::new();
        let errors = [
            Outcome::ServerError(503),
            Outcome::Network("connection reset".to_string()),
            Outcome::BadReply("release JSON".to_string()),
        ];
        for n in 0..10 {
            pacer.sent(now);
            pacer.replied(
                &reply(errors[n % errors.len()].clone(), RateLimit::default()),
                now,
            );
            let wait = pacer.wait(now);
            waits.push(wait.as_secs());
            now += wait;
        }
        assert_eq!(waits, [5, 10, 20, 40, 80, 160, 320, 600, 600, 600]);

        pacer.sent(now);
        pacer.replied(&reply(FOUND, RateLimit::default()), now);
        assert_eq!(pacer.wait(now), Duration::ZERO);
        pacer.sent(now);
        pacer.replied(&reply(Outcome::ServerError(500), RateLimit::default()), now);
        assert_eq!(pacer.wait(now), Duration::from_secs(5));
    }
}
