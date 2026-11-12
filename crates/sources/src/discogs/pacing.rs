//! Pacing for the Discogs API: how long to wait before the next request. It keeps its own
//! window of send times under the limit the replies report, spreads its requests evenly
//! over the window instead of sending them in bursts, waits when `Remaining` runs out,
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

/// How much longer than `WINDOW` our sends are counted as ours. Discogs counts a request
/// when it arrives, after we send it, so a send that has left our window can still be in
/// Discogs's. Counting too many as ours only misses another client's last few requests;
/// counting too few would take ours for another's.
const OURS_SLACK: Duration = Duration::from_secs(5);

/// The span of sends that count as ours when a reply's `Used` is compared with them, back from
/// the request the reply answers. It is also how long the pacer keeps sends (the sync records
/// them this long, and a new run seeds its pacer with them, [`Pacer::seed`]) and how long a
/// probe waits after our last send while yielding. The three are one value on purpose: the
/// previous probe has to fall out of ours when the next reply is read, or it would hide one
/// request of the other client.
pub const OURS_SPAN: Duration = WINDOW.saturating_add(OURS_SLACK);

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
    /// Send times within the last `OURS_SPAN`, oldest first.
    sent: VecDeque<SystemTime>,
    limit: u32,
    /// No request before this time: set by a 429, a backoff or `Remaining` running out.
    not_before: Option<SystemTime>,
    /// The last backoff, doubled on the next error and cleared by a reply that counts.
    backoff: Option<Duration>,
    /// The same for 429s, which count apart: their wait is also what `Retry-After` says. Any
    /// reply that isn't a 429 clears it.
    rate_limit_backoff: Option<Duration>,
    /// Whether the last reply with a `Used` showed another client's requests. While it is
    /// set, requests go out one `OURS_SPAN` apart.
    yielding: bool,
}

impl Default for Pacer {
    fn default() -> Self {
        Self {
            sent: VecDeque::new(),
            limit: DEFAULT_LIMIT,
            not_before: None,
            backoff: None,
            rate_limit_backoff: None,
            yielding: false,
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
        if let Some(&last) = self.sent.back() {
            // Evenly spaced, one window's limit spread over the window: a burst that refills
            // the window as each request turns 60 s old drew 429s from Discogs's moving
            // average with fewer than the limit in it.
            until = until.max(last + WINDOW / self.limit);
        }
        if self.yielding
            && let Some(&last) = self.sent.back()
        {
            // One request to see whether the other client has stopped, a span after our last
            // send, so the flag and the rate agree whatever the replies say.
            until = until.max(last + OURS_SPAN);
        }
        since(now, until)
    }

    /// Whether the last reply with a `Used` showed another client's requests, so the
    /// requests wait an `OURS_SPAN` between them until one shows none.
    #[must_use]
    pub fn yielding(&self) -> bool {
        self.yielding
    }

    /// Records that a request was sent at `now`.
    pub fn sent(&mut self, now: SystemTime) {
        self.sent.push_back(now);
        self.forget_old(now);
    }

    /// Records sends from before this pacer started, in any order: the last runs', as the
    /// community database recorded them, so Discogs's `Used` isn't taken to include another
    /// client's when it only counts ours. Call it at `now`, before the first `sent`.
    ///
    /// A send after `now` is taken as sent at `now`: the system clock has stepped back since
    /// it was recorded. Dropped, it would be taken for another client's; kept as it is, it
    /// would sit behind the next send, which `wait` and the count of ours read as the last.
    pub fn seed(&mut self, now: SystemTime, sends: impl IntoIterator<Item = SystemTime>) {
        let mut all: Vec<SystemTime> = self
            .sent
            .drain(..)
            .chain(sends)
            .map(|sent| sent.min(now))
            .collect();
        all.sort_unstable();
        self.sent = all.into();
        self.forget_old(now);
    }

    fn forget_old(&mut self, now: SystemTime) {
        while self
            .sent
            .front()
            .is_some_and(|&t| since(t, now) >= OURS_SPAN)
        {
            self.sent.pop_front();
        }
    }

    /// Records the reply to the last request, received at `now`.
    pub fn replied<T>(&mut self, reply: &Reply<T>, now: SystemTime) {
        if let Some(limit) = reply.rate_limit.limit.filter(|&limit| limit > 0) {
            self.limit = limit;
        }
        if !matches!(reply.outcome, Outcome::RateLimited(_)) {
            self.rate_limit_backoff = None;
        }
        // A reply without `Used` (trouble, a 429 without headers) says nothing about other
        // clients, so it neither starts a yield nor ends one.
        if let Some(used) = reply.rate_limit.used {
            self.yielding = used as usize > self.ours();
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
        // A reply's `Remaining` is the count before the request it answers, so 1 means
        // that request took the last one. Discogs's window frees a request when the oldest
        // one in it turns 60 s old.
        if matches!(reply.rate_limit.remaining, Some(0 | 1)) {
            // With no send of ours in it, the requests were someone else's with this
            // token, so wait a whole window.
            let oldest = self.in_window(now).first().copied().unwrap_or(now);
            self.wait_until(oldest + WINDOW);
        }
    }

    /// Our sends that Discogs can have counted in the reply to the last one: those within
    /// `OURS_SPAN` before it, the last one included. `Used` leaves out the request it
    /// answers, so with no other client ours is one more than it, which is no yield.
    fn ours(&self) -> usize {
        let Some(&last) = self.sent.back() else {
            return 0;
        };
        self.sent
            .iter()
            .filter(|&&t| since(t, last) < OURS_SPAN)
            .count()
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

    use super::{OURS_SPAN, Pacer};
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
        // One a second, never in a burst.
        for (n, pair) in sends.windows(2).enumerate() {
            assert!((pair[1] - pair[0] - 1.0).abs() < 1e-6, "{n}: {pair:?}");
        }
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
    fn waits_when_the_answered_request_took_the_last_one() {
        // Discogs's headers leave out the request they answer: the 60th request of a
        // window comes back with 1 remaining, and a 61st would be a 429.
        let mut pacer = Pacer::default();
        let mut now = at(0.0);
        pacer.sent(now);
        now = at(1.0);
        let last = RateLimit {
            limit: Some(60),
            remaining: Some(1),
            used: None,
        };
        pacer.replied(&reply(FOUND, last), now);
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
        // Only the second between requests.
        assert_eq!(pacer.wait(now), Duration::from_secs(1));
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
        // Only the second between requests.
        assert_eq!(pacer.wait(now), Duration::from_secs(1));
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

    /// Sends a request at `sent` seconds and has `reply` arrive 100 ms later.
    fn exchange(pacer: &mut Pacer, sent: f64, reply: &Reply) {
        pacer.sent(at(sent));
        pacer.replied(reply, at(sent + 0.1));
    }

    /// The wait left when a reply arrives 100 ms after the send that starts the span.
    fn span_after_reply() -> Duration {
        OURS_SPAN.saturating_sub(Duration::from_millis(100))
    }

    #[test]
    fn a_reply_showing_another_clients_requests_stops_sending_and_probes_once_every_ours_span() {
        let mut pacer = Pacer::default();
        // One request of ours in the window, and Discogs counts 12.
        exchange(&mut pacer, 0.0, &reply(FOUND, used(12)));
        assert!(pacer.yielding());
        // The probe is due `OURS_SPAN` after our last send.
        assert_eq!(pacer.wait(at(0.1)), span_after_reply());
        assert_eq!(
            pacer.wait(at(30.0)),
            OURS_SPAN.saturating_sub(Duration::from_secs(30))
        );
        // Each probe that still shows others waits another `OURS_SPAN`.
        let mut sent = 65.0;
        for used_now in [30, 9, 3] {
            assert_eq!(pacer.wait(at(sent)), Duration::ZERO);
            exchange(&mut pacer, sent, &reply(FOUND, used(used_now)));
            assert!(pacer.yielding(), "{sent}");
            assert_eq!(pacer.wait(at(sent + 0.1)), span_after_reply());
            sent += 65.0;
        }
    }

    #[test]
    fn a_reply_showing_no_one_else_resumes() {
        let mut pacer = Pacer::default();
        pacer.sent(at(0.0));
        pacer.replied(&reply(FOUND, used(12)), at(0.1));
        pacer.sent(at(65.0));
        // Only the probe in Discogs's window: the other client has stopped.
        pacer.replied(&reply(FOUND, used(1)), at(65.1));
        assert!(!pacer.yielding());
        // Back to one a second.
        assert_eq!(pacer.wait(at(65.1)), Duration::from_millis(900));
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
        // A probe that times out, one the API refuses for the moment (a 429 waits a minute,
        // less than `OURS_SPAN`) and a 200 without the header: none says anything about
        // the other client.
        let outcomes = [trouble(), Outcome::RateLimited(None), FOUND];
        let mut pacer = Pacer::default();
        exchange(&mut pacer, 0.0, &reply(FOUND, used(12)));
        assert!(pacer.yielding());
        let mut sent = 0.0;
        for outcome in outcomes {
            sent += 65.0;
            exchange(&mut pacer, sent, &reply(outcome, RateLimit::default()));
            assert!(pacer.yielding(), "{sent}");
            // The next probe is `OURS_SPAN` after the last send, so the pacer doesn't
            // send at full speed while the sync says it yields.
            assert_eq!(pacer.wait(at(sent + 0.1)), span_after_reply(), "{sent}");
        }
        // A reply with the header decides.
        exchange(&mut pacer, sent + 65.0, &reply(FOUND, used(1)));
        assert!(!pacer.yielding());
    }

    #[test]
    fn a_lone_request_of_another_client_between_probes_keeps_the_yield() {
        let mut pacer = Pacer::default();
        pacer.sent(at(0.0));
        pacer.replied(&reply(FOUND, used(12)), at(0.1));
        // The next probe goes out when the pacer says, so our previous send is out of both
        // Discogs's window and ours. Discogs counts the probe and one request of the other
        // client.
        let probe = at(0.1) + pacer.wait(at(0.1));
        pacer.sent(probe);
        pacer.replied(&reply(FOUND, used(2)), probe + Duration::from_millis(100));
        assert!(pacer.yielding());
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
            assert!(sends[29] < 30.0, "{}", sends[29]);
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
        pacer.seed(
            at(0.0),
            (0..60).rev().map(|n| at(f64::from(n) * 0.1 - 10.0)),
        );
        assert_eq!(pacer.wait(at(0.0)), Duration::from_secs(50));
        pacer.sent(at(50.0));
        pacer.replied(&reply(FOUND, used(61)), at(50.1));
        assert!(!pacer.yielding());
        // Sends older than a window and the slack are dropped.
        let mut pacer = Pacer::default();
        pacer.seed(at(0.0), [at(-100.0), at(-20.0)]);
        pacer.sent(at(0.0));
        pacer.replied(&reply(FOUND, used(3)), at(0.1));
        assert!(pacer.yielding());
    }

    #[test]
    fn a_seed_keeps_the_sends_oldest_first_and_none_after_now() {
        let mut pacer = Pacer::default();
        // A send recorded at 30 s, before the system clock stepped back to 10 s.
        pacer.seed(at(10.0), [at(5.0), at(30.0)]);
        pacer.sent(at(10.0));
        assert!(pacer.sent.iter().is_sorted(), "{:?}", pacer.sent);
        assert_eq!(pacer.sent.back(), Some(&at(10.0)));
        // A yield's probe goes out an interval after the send just made, not the later one.
        pacer.replied(&reply(FOUND, used(12)), at(10.1));
        assert_eq!(pacer.wait(at(10.1)), span_after_reply());
    }
}
