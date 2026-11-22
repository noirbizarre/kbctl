//! Host-driven input discovery; future CLI/daemon hosts share this policy.
#![allow(dead_code)]

use crate::{
    model::state::{CommitOutcome, StateError},
    model::{identity::AdvertisedSerial, input::*},
    registry::{PhysicalMetadata, Registry},
    runtime::{Runtime, RuntimeError},
};
use std::time::{Duration, Instant};

pub(crate) mod bitmap;

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Drain {
    pub(crate) changed: bool,
    pub(crate) pending: bool,
}

pub(crate) trait Source {
    fn start_monitor(&mut self) -> Result<(), Problem>;
    fn drain(&mut self, budget: usize) -> Result<Drain, Problem>;
    fn scan(&mut self) -> Scan;
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Config {
    pub(crate) period: Duration,
    pub(crate) retry: Duration,
    pub(crate) max_retry: Duration,
    pub(crate) notification_budget: usize,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            period: Duration::from_secs(30),
            retry: Duration::from_secs(1),
            max_retry: Duration::from_secs(30),
            notification_budget: 64,
        }
    }
}

pub(crate) struct Service<S> {
    source: S,
    registry: Registry,
    config: Config,
    monitor: MonitorStatus,
    next_scan: Instant,
    next_monitor: Instant,
    backoff: Duration,
    dirty: bool,
    last_step: Instant,
}
pub(crate) struct Outcome {
    pub(crate) publication: Option<CommitOutcome>,
    pub(crate) problems: Vec<Problem>,
}
impl<S: Source> Service<S> {
    pub(crate) fn new(
        source: S,
        registry: Registry,
        config: Config,
        now: Instant,
    ) -> Result<Self, Problem> {
        if config.period.is_zero()
            || config.retry.is_zero()
            || config.max_retry < config.retry
            || config.notification_budget == 0
        {
            return Err(Problem {
                path: "input discovery configuration".into(),
                operation: "validate positive scheduling bounds".into(),
                kind: ProblemKind::Malformed,
                os_code: None,
            });
        }
        if now.checked_add(config.period).is_none() || now.checked_add(config.max_retry).is_none() {
            return Err(Problem {
                path: "input discovery configuration".into(),
                operation: "validate representable scheduling bounds".into(),
                kind: ProblemKind::Malformed,
                os_code: None,
            });
        }
        Ok(Self {
            source,
            registry,
            config,
            monitor: MonitorStatus::NotStarted,
            next_scan: now,
            next_monitor: now,
            backoff: config.retry,
            dirty: true,
            last_step: now,
        })
    }
    pub(crate) fn source(&self) -> &S {
        &self.source
    }
    pub(crate) fn next_deadline(&self) -> Instant {
        if self.dirty {
            return self.last_step;
        }
        if matches!(self.monitor, MonitorStatus::Watching) {
            self.next_scan
        } else {
            self.next_scan.min(self.next_monitor)
        }
    }
    pub(crate) fn step(
        &mut self,
        now: Instant,
        runtime: &mut Runtime,
    ) -> Result<Outcome, RuntimeError> {
        // Reject bad time before performing externally visible source work.
        if now > runtime.now() {
            return Err(StateError::Clock("discovery action is in the future").into());
        }
        if now < self.last_step {
            return Err(StateError::Clock("discovery action time regressed").into());
        }
        // Validate deadline arithmetic before consuming notifications or publishing a scan.
        if now.checked_add(self.config.period).is_none()
            || now.checked_add(self.config.max_retry).is_none()
        {
            return Err(StateError::Clock("discovery scheduling deadline overflowed").into());
        }
        self.last_step = now;
        let mut problems = Vec::new();
        if !matches!(self.monitor, MonitorStatus::Watching) && now >= self.next_monitor {
            self.monitor = match self.source.start_monitor() {
                Ok(()) => MonitorStatus::Watching,
                Err(problem) => {
                    problems.push(problem.clone());
                    MonitorStatus::Degraded(problem)
                }
            };
            self.next_monitor = now + self.backoff;
            self.dirty = true;
        }
        let mut pending = false;
        if matches!(self.monitor, MonitorStatus::Watching) {
            match self.source.drain(self.config.notification_budget) {
                Ok(drain) => {
                    // Even an all-interrupted batch needs another turn; a drain budget is not proof of idle.
                    self.dirty |= drain.changed || drain.pending;
                    pending = drain.pending;
                }
                Err(problem) => {
                    self.monitor = MonitorStatus::Degraded(problem.clone());
                    problems.push(problem);
                    self.next_monitor = now + self.backoff;
                    self.dirty = true;
                }
            }
        }
        if !self.dirty && now < self.next_scan {
            return Ok(Outcome {
                publication: None,
                problems,
            });
        }
        // Keep dirty until publication succeeds, including when the runtime rejects its timestamp/reservation.
        self.dirty = true;
        let mut scan = self.source.scan();
        for group in &mut scan.groups {
            let serial = group
                .serial
                .known()
                .and_then(|serial| AdvertisedSerial::new(serial.clone()).ok());
            group.identification = self
                .registry
                .identify_metadata(PhysicalMetadata {
                    usb: group.usb.known().copied(),
                    serial: serial.as_ref(),
                })
                .identification;
        }
        let successful = scan.complete
            && scan.problems.is_empty()
            && matches!(self.monitor, MonitorStatus::Watching);
        problems.extend(scan.problems.iter().cloned());
        let publication = runtime.apply_input_scan(scan, self.monitor.clone(), now)?;
        self.dirty = false;
        let delay = if successful {
            self.backoff = self.config.retry;
            self.config.period
        } else {
            let delay = self.backoff;
            self.backoff = self.backoff.saturating_mul(2).min(self.config.max_retry);
            delay
        };
        self.next_scan = if pending { now } else { now + delay };
        Ok(Outcome {
            publication: Some(publication),
            problems,
        })
    }
}

pub(crate) fn classify(
    property: &Metadata<String>,
    capabilities: &Capabilities,
) -> KeyboardClassification {
    if let Some(value) = property.known() {
        match value.as_str() {
            "1" => {
                return KeyboardClassification::Known {
                    keyboard: true,
                    evidence: ClassificationEvidence::Udev,
                };
            }
            "0" => {
                return KeyboardClassification::Known {
                    keyboard: false,
                    evidence: ClassificationEvidence::Udev,
                };
            }
            _ => {}
        }
    }
    let Some(events) = capabilities.events.known() else {
        return KeyboardClassification::Unknown;
    };
    if !events.contains(&1) {
        return KeyboardClassification::Known {
            keyboard: false,
            evidence: ClassificationEvidence::KeyBitmap,
        };
    }
    let Some(keys) = capabilities.codes.get(&1).and_then(Metadata::known) else {
        return KeyboardClassification::Unknown;
    };
    // EV_KEY alone also covers mice/buttons. This conservative ordinary-key row is not a universal keyboard proof.
    KeyboardClassification::Known {
        keyboard: (16..=25).all(|key| keys.contains(&key)),
        evidence: ClassificationEvidence::KeyBitmap,
    }
}

#[cfg(test)]
mod tests;
