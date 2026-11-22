use crate::model::time::Clock;
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

pub(crate) struct ManualClock {
    origin: Instant,
    now: Mutex<Instant>,
}
impl ManualClock {
    pub(crate) fn new() -> Self {
        let origin = Instant::now();
        Self {
            origin,
            now: Mutex::new(origin),
        }
    }
    pub(crate) fn time(&self, seconds: u64) -> Instant {
        self.origin + Duration::from_secs(seconds)
    }
    pub(crate) fn at(&self, seconds: u64) -> Instant {
        let time = self.time(seconds);
        *self.now.lock().unwrap() = time;
        time
    }
    pub(crate) fn advance(&self, duration: Duration) {
        *self.now.lock().unwrap() += duration;
    }
    /// Explicitly invalid time is useful for checking production clock guards.
    pub(crate) fn rewind(&self, duration: Duration) {
        *self.now.lock().unwrap() -= duration;
    }
}
impl Default for ManualClock {
    fn default() -> Self {
        Self::new()
    }
}
impl Clock for ManualClock {
    fn now(&self) -> Instant {
        *self.now.lock().unwrap()
    }
}
