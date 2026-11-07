//! The clock the sync reads and sleeps on, passed in so tests can use a made-up one.

use std::time::{Duration, SystemTime};

/// The time, and a way to wait.
pub trait Clock {
    /// The current time.
    fn now(&self) -> SystemTime;
    /// Waits `duration`. A made-up clock advances its time instead.
    fn sleep(&self, duration: Duration);
}

/// The system's clock and `thread::sleep`.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// Seconds since the Unix epoch, the community database's time. A time before the epoch
/// counts as negative.
pub(crate) fn unix_seconds(time: SystemTime) -> i64 {
    match time.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(after) => i64::try_from(after.as_secs()).unwrap_or(i64::MAX),
        Err(before) => i64::try_from(before.duration().as_secs()).map_or(i64::MIN, |s| -s),
    }
}
