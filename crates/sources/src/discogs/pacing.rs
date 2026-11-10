//! Pacing for the Discogs API: how long to wait before the next request. It keeps its own
//! window of send times under the limit the replies report, waits when `Remaining` is 0,
//! waits out a 429, backs off after trouble (no reply, a 5xx, a 408, a 425), and yields to
//! another client sharing the limit, the player's Discogs tagger (ADR 0020).
//!
//! It holds no clock and never sleeps: the caller passes the time in and does the waiting,
//! so tests drive it with made-up times.

use std::collections::VecDeque;
use std::time::{Duration, SystemTime};

use super::api::{Outcome, Reply};

/// Discogs counts requests in a moving window this long.
pub const WINDOW: Duration = Duration::from_secs(60);

/// The authenticated limit, used until a reply reports one.
pub const DEFAULT_LIMIT: u32 = 60;

/// How much longer than `WINDOW` before the request answered our sends are counted as ours
/// when a reply's `Used` is compared with them. Discogs counts a request when it arrives,
/// after we send it, so a send that has left our window can still be in Discogs's. Counting
/// too many as ours only misses another client's last few requests; counting too few would
/// take ours for another's.
const OURS_SLACK: Duration = Duration::from_secs(5);

/// How far back the pacer keeps send times. A new run seeds its pacer with the sends from
/// this long before it starts ([`Pacer::seed`]).
pub const SENDS_KEPT: Duration = WINDOW.saturating_add(OURS_SLACK);

/// A 429 without `Retry-After` waits this long.
const RATE_LIMITED_WAIT: Duration = Duration::from_secs(60);

/// The first wait after a 5xx, a 408, a 425, a network error or a 429, doubled for each one
/// after it. For a 429 it is the least wait, so a `Retry-After` of 0 doesn't send again at
/// once.
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
    /// The same for 429s, which count apart: their wait is also what `Retry-After` says. Any
    /// reply that isn't a 429 clears it.
    rate_limit_backoff: Option<Duration>,
    /// When the last reply with a `Used` came, if it showed another client's requests.
    /// `Some` while yielding: the next request waits until a window has passed since then.
    others_at: Option<SystemTime>,
}

impl Default for Pacer {
    fn default() -> Self {
        Self {
            sent: VecDeque::new(),
            limit: DEFAULT_LIMIT,
            not_before: None,
            backoff: None,
            rate_limit_backoff: None,
            others_at: None,
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
        if let Some(others_at) = self.others_at {
            // Nothing while another client has sent in the last window, then one request
            // to see whether it has stopped.
            until = until.max(others_at + WINDOW);
        }
        since(now, until)
    }

    /// Whether the last reply showed another client's requests in the window, so the
    /// requests wait a window between them until one shows none.
    #[must_use]
    pub fn yielding(&self) -> bool {
        self.others_at.is_some()
    }

    /// Records that a request was sent at `now`.
    pub fn sent(&mut self, now: SystemTime) {
        self.sent.push_back(now);
        self.forget_old(now);
    }

    /// Records sends from before this pacer started, in any order: the last run's, as the
    /// community database holds them, so Discogs's `Used` isn't taken to include another
    /// client's when it only counts ours. Call it before the first `sent`.
    pub fn seed(&mut self, sends: impl IntoIterator<Item = SystemTime>) {
        let mut all: Vec<SystemTime> = self.sent.drain(..).chain(sends).collect();
        all.sort_unstable();
        self.sent = all.into();
        if let Some(&last) = self.sent.back() {
            self.forget_old(last);
        }
    }

    fn forget_old(&mut self, now: SystemTime) {
        while self
            .sent
            .front()
            .is_some_and(|&t| since(t, now) >= SENDS_KEPT)
        {
            self.sent.pop_front();
        }
    }

    /// Records the reply to the last request, received at `now`.
    pub fn replied(&mut self, reply: &Reply, now: SystemTime) {
        if let Some(limit) = reply.rate_limit.limit.filter(|&limit| limit > 0) {
            self.limit = limit;
        }
        if !matches!(reply.outcome, Outcome::RateLimited(_)) {
            self.rate_limit_backoff = None;
        }
        // A reply without `Used` (trouble, a 429 without headers) says nothing about other
        // clients, so it neither starts a yield nor ends one.
        if let Some(used) = reply.rate_limit.used {
            self.others_at = (used > self.ours()).then_some(now);
        }
        let wait = match &reply.outcome {
            // A reply about the release (a body or status the API doesn't send, which the
            // sync defers that release for) says nothing about Discogs's load, and a refused
            // token ends the run, so neither backs off the requests for the others.
            Outcome::Found { .. }
            | Outcome::NotFound
            | Outcome::Refused(_)
            | Outcome::ReleaseProblem(_) => {
                self.backoff = None;
                None
            }
            Outcome::RateLimited(retry_after) => {
                let backoff = next_backoff(self.rate_limit_backoff);
                self.rate_limit_backoff = Some(backoff);
                Some(retry_after.unwrap_or(RATE_LIMITED_WAIT).max(backoff))
            }
            Outcome::Trouble(_) => {
                let backoff = next_backoff(self.backoff);
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

    /// Our sends that Discogs can have counted in the reply to the last one: those from a
    /// window and `OURS_SLACK` before it, the last one included. `Used` counts the request
    /// it answers, and if it doesn't, ours is one more than it, which is no yield either.
    fn ours(&self) -> u32 {
        let Some(&last) = self.sent.back() else {
            return 0;
        };
        let counted = self
            .sent
            .iter()
            .filter(|&&t| since(t, last) <= WINDOW + OURS_SLACK)
            .count();
        u32::try_from(counted).unwrap_or(u32::MAX)
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

/// The wait after the one before it: `FIRST_BACKOFF`, doubled each time up to `MAX_BACKOFF`.
fn next_backoff(last: Option<Duration>) -> Duration {
    last.map_or(FIRST_BACKOFF, |last| (last * 2).min(MAX_BACKOFF))
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

    const FOUND: Outcome = Outcome::Found {
        values: Community {
            rating_average: 4.0,
            rating_count: 1,
            have: 1,
            want: 1,
            num_for_sale: None,
            lowest_price: None,
        },
        body: String::new(),
    };

    fn trouble() -> Outcome {
        Outcome::Trouble("Discogs answered 503".to_string())
    }

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
            remaining: None,
            used: None,
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
            remaining: Some(0),
            used: None,
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
    fn consecutive_429s_wait_at_least_5_s_doubling_and_reset_on_success() {
        let mut pacer = Pacer::default();
        let mut now = at(0.0);
        let mut waits = Vec::new();
        for _ in 0..12 {
            pacer.sent(now);
            let limited = Outcome::RateLimited(Some(Duration::ZERO));
            pacer.replied(&reply(limited, RateLimit::default()), now);
            let wait = pacer.wait(now);
            waits.push(wait.as_secs());
            now += wait;
        }
        assert_eq!(
            waits,
            [5, 10, 20, 40, 80, 160, 320, 600, 600, 600, 600, 600]
        );

        pacer.sent(now);
        pacer.replied(&reply(FOUND, RateLimit::default()), now);
        pacer.sent(now);
        let limited = Outcome::RateLimited(Some(Duration::ZERO));
        pacer.replied(&reply(limited, RateLimit::default()), now);
        assert_eq!(pacer.wait(now), Duration::from_secs(5));

        // A longer Retry-After still wins.
        let limited = Outcome::RateLimited(Some(Duration::from_mins(15)));
        pacer.replied(&reply(limited, RateLimit::default()), now);
        assert_eq!(pacer.wait(now), Duration::from_mins(15));
    }

    #[test]
    fn any_reply_but_a_429_resets_the_429_floor() {
        let mut pacer = Pacer::default();
        let mut now = at(0.0);
        let mut waits = Vec::new();
        let replies = [
            Outcome::RateLimited(None),
            Outcome::RateLimited(Some(Duration::ZERO)),
            Outcome::RateLimited(Some(Duration::ZERO)),
            trouble(),
            Outcome::RateLimited(Some(Duration::ZERO)),
        ];
        for outcome in replies {
            pacer.sent(now);
            pacer.replied(&reply(outcome, RateLimit::default()), now);
            let wait = pacer.wait(now);
            waits.push(wait.as_secs());
            now += wait;
        }
        // The 429 after the 503 waits 5 s again, not 40: the 503's backoff is 5 s too.
        assert_eq!(waits, [60, 10, 20, 5, 5]);
    }

    #[test]
    fn backoff_doubles_to_ten_minutes_and_resets_on_success() {
        let mut pacer = Pacer::default();
        let mut now = at(0.0);
        let mut waits = Vec::new();
        let errors = [trouble(), trouble()];
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
        pacer.replied(&reply(trouble(), RateLimit::default()), now);
        assert_eq!(pacer.wait(now), Duration::from_secs(5));
    }

    #[test]
    fn a_bad_reply_about_the_release_does_not_slow_the_pace() {
        let mut pacer = Pacer::default();
        let now = at(0.0);
        pacer.sent(now);
        let bad = Outcome::ReleaseProblem("unexpected status 410".to_string());
        pacer.replied(&reply(bad, RateLimit::default()), now);
        assert_eq!(pacer.wait(now), Duration::ZERO);
        // And it ends a backoff, as a reply from Discogs that isn't trouble.
        pacer.sent(now);
        pacer.replied(&reply(trouble(), RateLimit::default()), now);
        assert_eq!(pacer.wait(now), Duration::from_secs(5));
        let bad = Outcome::ReleaseProblem("release JSON".to_string());
        pacer.replied(&reply(bad, RateLimit::default()), at(5.0));
        pacer.replied(&reply(trouble(), RateLimit::default()), at(5.0));
        assert_eq!(pacer.wait(at(5.0)), Duration::from_secs(5));
    }

    /// A reply's headers with `used` in `X-Discogs-Ratelimit-Used`.
    fn used(used: u32) -> RateLimit {
        RateLimit {
            limit: Some(60),
            remaining: Some(60_u32.saturating_sub(used)),
            used: Some(used),
        }
    }

    #[test]
    fn a_reply_showing_another_clients_requests_stops_sending_and_probes_once_a_minute() {
        let mut pacer = Pacer::default();
        pacer.sent(at(0.0));
        // One request of ours in the window, and Discogs counts 12.
        pacer.replied(&reply(FOUND, used(12)), at(0.1));
        assert!(pacer.yielding());
        assert_eq!(pacer.wait(at(0.1)), Duration::from_mins(1));
        assert_eq!(pacer.wait(at(30.1)), Duration::from_secs(30));
        // Each probe that still shows others waits another minute.
        for (sent, used_now) in [(60.1, 30), (120.2, 9), (180.3, 3)] {
            assert_eq!(pacer.wait(at(sent)), Duration::ZERO);
            pacer.sent(at(sent));
            pacer.replied(&reply(FOUND, used(used_now)), at(sent + 0.1));
            assert!(pacer.yielding(), "{sent}");
            assert_eq!(pacer.wait(at(sent + 0.1)), Duration::from_mins(1), "{sent}");
        }
    }

    #[test]
    fn a_reply_showing_no_one_else_resumes() {
        let mut pacer = Pacer::default();
        pacer.sent(at(0.0));
        pacer.replied(&reply(FOUND, used(12)), at(0.1));
        pacer.sent(at(60.1));
        // Only the probe in Discogs's window: the other client has stopped.
        pacer.replied(&reply(FOUND, used(1)), at(60.2));
        assert!(!pacer.yielding());
        assert_eq!(pacer.wait(at(60.2)), Duration::ZERO);
    }

    #[test]
    fn a_reply_without_the_used_header_does_not_start_a_yield() {
        let mut pacer = Pacer::default();
        pacer.sent(at(0.0));
        pacer.replied(&reply(FOUND, RateLimit::default()), at(0.1));
        assert!(!pacer.yielding());
        pacer.sent(at(0.2));
        pacer.replied(&reply(trouble(), RateLimit::default()), at(0.3));
        assert!(!pacer.yielding());
        // What is left is trouble's backoff.
        assert_eq!(pacer.wait(at(0.3)), Duration::from_secs(5));
    }

    #[test]
    fn a_reply_without_the_used_header_does_not_end_a_yield() {
        let mut pacer = Pacer::default();
        pacer.sent(at(0.0));
        pacer.replied(&reply(FOUND, used(12)), at(0.1));
        assert!(pacer.yielding());
        // The probe times out: nothing is known about the other client.
        pacer.sent(at(60.1));
        pacer.replied(&reply(trouble(), RateLimit::default()), at(60.2));
        assert!(pacer.yielding());
        // Another minute from the reply that showed it, not from the trouble.
        assert_eq!(pacer.wait(at(60.2)), Duration::from_secs(5));
        pacer.sent(at(65.2));
        pacer.replied(
            &reply(Outcome::RateLimited(None), RateLimit::default()),
            at(65.3),
        );
        assert!(pacer.yielding());
        // A 200 without the header leaves it too, and the next probe is still due at
        // the minute after the last reply that showed another client.
        pacer.sent(at(125.3));
        pacer.replied(&reply(FOUND, RateLimit::default()), at(125.4));
        assert!(pacer.yielding());
        assert_eq!(pacer.wait(at(125.4)), Duration::ZERO);
        // A reply with the header decides.
        pacer.sent(at(125.5));
        pacer.replied(&reply(FOUND, used(2)), at(125.6));
        assert!(!pacer.yielding());
    }

    #[test]
    fn our_own_requests_never_count_as_another_clients() {
        // 30 of our sends in the window, with Discogs counting the request it answers or not.
        for counts_this_one in [1, 0] {
            let mut pacer = Pacer::default();
            let sends = run(&mut pacer, 30, |n| {
                let ours = u32::try_from(n).unwrap() + counts_this_one;
                reply(FOUND, used(ours))
            });
            assert!(!pacer.yielding());
            assert!(sends[29] < 3.0, "{}", sends[29]);
        }

        // At the limit for minutes, with Discogs counting each request when it arrives,
        // 50 ms after it is sent: sends that leave our window between a send and its reply
        // are still in Discogs's.
        let mut pacer = Pacer::default();
        let mut now = at(0.0);
        let mut sends = Vec::new();
        for _ in 0..300 {
            now += pacer.wait(now);
            pacer.sent(now);
            sends.push(now);
            let arrived = now + Duration::from_millis(50);
            let counted = sends
                .iter()
                .filter(|&&sent| arrived < sent + Duration::from_millis(50) + super::WINDOW)
                .count();
            now += Duration::from_millis(100);
            pacer.replied(&reply(FOUND, used(u32::try_from(counted).unwrap())), now);
            assert!(!pacer.yielding(), "{counted} at {now:?}");
        }

        // A reply that takes 1.1 s: our first send has left our window when it comes, but
        // Discogs counted both.
        let mut pacer = Pacer::default();
        pacer.sent(at(0.0));
        pacer.replied(&reply(FOUND, used(1)), at(0.1));
        pacer.sent(at(59.9));
        pacer.replied(&reply(FOUND, used(2)), at(61.0));
        assert!(!pacer.yielding());
    }

    #[test]
    fn seeded_sends_count_as_ours_and_toward_the_limit() {
        let mut pacer = Pacer::default();
        // The last run's 60 sends, given out of order, then this run's first reply, which
        // Discogs counts as the 61st.
        pacer.seed((0..60).rev().map(|n| at(f64::from(n) * 0.1 - 10.0)));
        assert_eq!(pacer.wait(at(0.0)), Duration::from_secs(50));
        pacer.sent(at(50.0));
        pacer.replied(&reply(FOUND, used(61)), at(50.1));
        assert!(!pacer.yielding());
        // Sends older than a window and the slack are dropped.
        let mut pacer = Pacer::default();
        pacer.seed([at(-100.0), at(-20.0)]);
        pacer.sent(at(0.0));
        pacer.replied(&reply(FOUND, used(3)), at(0.1));
        assert!(pacer.yielding());
    }
}
