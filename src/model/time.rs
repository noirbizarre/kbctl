//! One host monotonic time domain; firmware clock labels never imply synchronization.

use std::time::{Duration, Instant};

pub(crate) trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}
pub(crate) struct SystemClock;
impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Duration from runtime startup, not wall time or a hardware ordering key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub(crate) struct Timestamp(pub(crate) Duration);
impl Timestamp {
    pub(crate) const ZERO: Self = Self(Duration::ZERO);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IngressOrigin {
    Reply,
    Event,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IngressStamp {
    pub(crate) sequence: u64,
    pub(crate) received_at: Instant,
    pub(crate) origin: IngressOrigin,
}
