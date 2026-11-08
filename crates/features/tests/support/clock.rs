//! A made-up clock for the features tests, which include this file with `#[path]`: it
//! starts at a fixed time and moves only when slept on, so nothing waits on real time.

#![allow(
    dead_code,
    reason = "each test crate that includes this file uses part of it"
)]

use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use features::clock::Clock;

/// The made-up clock's start, in Unix seconds.
pub(crate) const START: i64 = 1_800_000_000;

/// A clock that starts at [`START`] and moves only when slept on.
pub(crate) struct FakeClock {
    now: Mutex<SystemTime>,
    sleeps: Mutex<Vec<Duration>>,
    /// Called with the number of sleeps so far, after each one.
    pub(crate) on_sleep: Box<dyn Fn(usize) + Send + Sync>,
}

impl FakeClock {
    pub(crate) fn new() -> Self {
        Self {
            now: Mutex::new(SystemTime::UNIX_EPOCH + Duration::from_secs(START.unsigned_abs())),
            sleeps: Mutex::new(Vec::new()),
            on_sleep: Box::new(|_| {}),
        }
    }

    /// Every sleep so far, in order.
    pub(crate) fn sleeps(&self) -> Vec<Duration> {
        self.sleeps.lock().unwrap().clone()
    }

    /// The time slept so far.
    pub(crate) fn slept(&self) -> Duration {
        self.sleeps().iter().sum()
    }

    /// The time in Unix seconds.
    pub(crate) fn seconds(&self) -> i64 {
        let since = self.now().duration_since(SystemTime::UNIX_EPOCH).unwrap();
        i64::try_from(since.as_secs()).unwrap()
    }
}

impl Clock for FakeClock {
    fn now(&self) -> SystemTime {
        *self.now.lock().unwrap()
    }

    fn sleep(&self, duration: Duration) {
        let count = {
            let mut sleeps = self.sleeps.lock().unwrap();
            sleeps.push(duration);
            sleeps.len()
        };
        *self.now.lock().unwrap() += duration;
        (self.on_sleep)(count);
    }
}
