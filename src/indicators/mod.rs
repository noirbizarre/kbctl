//! Endpoint-scoped host indicator collection; notifications invalidate snapshots, never define lock truth.
#![allow(dead_code)]
use crate::{
    model::{
        identity::InputEndpointId,
        input::InputEndpoint,
        observation::{HostLock, NamespacedId},
        state::{CommitOutcome, HostPatch, HostUnavailable, HostValue, StateError},
    },
    runtime::{HostHandle, Runtime, RuntimeError},
};
use miette::Diagnostic;
use std::{
    collections::BTreeMap,
    io,
    path::PathBuf,
    time::{Duration, Instant},
};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fallback {
    Kernel,
    Setup,
    RecoveryTimeout,
    StreamFailure,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Events,
    Snapshots(Fallback),
    Unsupported,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ErrorKind {
    Permission,
    Inaccessible,
    Unsupported,
    Binding,
    Protocol,
}
#[derive(Debug, Error, Diagnostic)]
#[error("cannot {operation} for host indicators at {} ({kind:?})", path.display())]
#[diagnostic(
    code(kbctl::indicators::access),
    help(
        "Input discovery can succeed without indicator access. Check this specific node, its current attachment and deliberately configured seat/ACL policy; retry after correcting access. A notification setup limitation can use snapshot fallback. Do not run kbctl as root or grant broad input access by default."
    )
)]
pub(crate) struct AccessError {
    pub(crate) path: PathBuf,
    pub(crate) operation: &'static str,
    pub(crate) kind: ErrorKind,
    #[source]
    pub(crate) source: Option<io::Error>,
}
impl AccessError {
    pub(crate) fn unavailable(&self) -> HostUnavailable {
        match self.kind {
            ErrorKind::Permission => HostUnavailable::PermissionDenied,
            ErrorKind::Unsupported => HostUnavailable::Unsupported,
            _ => HostUnavailable::Inaccessible,
        }
    }
}
#[derive(Debug, Error, Diagnostic)]
pub(crate) enum Error {
    #[error(transparent)]
    #[diagnostic(transparent)]
    Runtime(#[from] RuntimeError),
    #[error("invalid host indicator configuration: {0}")]
    #[diagnostic(
        code(kbctl::indicators::configuration),
        help(
            "Use positive bounded work/retry limits and refresh intervals of at least one second, with representable finite freshness deadlines."
        )
    )]
    Config(&'static str),
    #[error("host indicator receipt or lease identifiers are exhausted")]
    #[diagnostic(
        code(kbctl::indicators::exhausted),
        help("Stop this collector and restart kbctl; identifiers and sequences must never wrap.")
    )]
    Exhausted,
}

#[derive(Debug, Clone)]
pub(crate) struct Snapshot {
    pub(crate) caps: HostValue,
    pub(crate) num: HostValue,
    pub(crate) scroll: HostValue,
}
impl Snapshot {
    pub(crate) fn patch(&self) -> HostPatch {
        HostPatch(BTreeMap::from([
            (HostLock::Caps, self.caps),
            (HostLock::Num, self.num),
            (HostLock::Scroll, self.scroll),
        ]))
    }
}
#[derive(Debug, Default)]
pub(crate) struct Drain {
    pub(crate) changed: bool,
    pub(crate) gap: bool,
    pub(crate) synchronized: bool,
    pub(crate) pending: bool,
    pub(crate) processed: usize,
}
pub(crate) trait Lease {
    fn mode(&self) -> Mode;
    fn snapshot(&mut self) -> Result<Snapshot, AccessError>;
    fn drain(&mut self, budget: usize) -> Result<Drain, AccessError>;
    fn downgrade(&mut self, reason: Fallback);
    fn take_warning(&mut self) -> Option<AccessError> {
        None
    }
}
pub(crate) trait Opener {
    type Lease: Lease;
    fn open(&mut self, endpoint: &InputEndpoint) -> Result<Self::Lease, AccessError>;
}
#[derive(Debug, Clone, Copy)]
pub(crate) struct Config {
    pub(crate) snapshot: Duration,
    pub(crate) health: Duration,
    pub(crate) retry: Duration,
    pub(crate) max_retry: Duration,
    pub(crate) recovery: Duration,
    pub(crate) endpoints: usize,
    pub(crate) events_per_endpoint: usize,
    pub(crate) events: usize,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            snapshot: Duration::from_secs(1),
            health: Duration::from_secs(30),
            retry: Duration::from_secs(1),
            max_retry: Duration::from_secs(30),
            recovery: Duration::from_secs(1),
            endpoints: 32,
            events_per_endpoint: 64,
            events: 256,
        }
    }
}
impl Config {
    fn interval(self, mode: Mode) -> Duration {
        if matches!(mode, Mode::Snapshots(_)) {
            self.snapshot
        } else {
            self.health
        }
    }
    fn validate(self, now: Instant) -> Result<(), Error> {
        if self.snapshot < Duration::from_secs(1)
            || self.health < self.snapshot
            || self.retry.is_zero()
            || self.max_retry < self.retry
            || self.recovery.is_zero()
            || self.endpoints == 0
            || self.events_per_endpoint == 0
            || self.events == 0
        {
            return Err(Error::Config("invalid cadence or budget"));
        }
        for duration in [self.snapshot, self.health, self.max_retry, self.recovery] {
            if duration
                .checked_mul(3)
                .and_then(|d| now.checked_add(d))
                .is_none()
            {
                return Err(Error::Config("deadline overflow"));
            }
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Ready {
    pub(crate) endpoint: InputEndpointId,
    pub(crate) generation: u64,
    pub(crate) closed: bool,
}
struct Receipt {
    value: Snapshot,
    sequence: u64,
    received: Instant,
    until: Instant,
}
struct Recovery {
    until: Instant,
    synchronized: bool,
}
struct Entry<L> {
    endpoint: InputEndpoint,
    handle: Option<HostHandle>,
    lease: Option<L>,
    generation: u64,
    sequence: u64,
    pending: Option<Receipt>,
    failure: Option<HostUnavailable>,
    unavailable: bool,
    next: Instant,
    backoff: Duration,
    ready: bool,
    drain_pending: bool,
    recovery: Option<Recovery>,
    retiring: bool,
}
pub(crate) struct Service<O: Opener> {
    opener: O,
    config: Config,
    entries: BTreeMap<InputEndpointId, Entry<O::Lease>>,
    cursor: usize,
    generation: u64,
    last_step: Instant,
    stopping: bool,
    scope: crate::input::classification::CollectionScope,
    pending_admission: bool,
}
pub(crate) struct Outcome {
    pub(crate) publications: Vec<CommitOutcome>,
    pub(crate) problems: Vec<AccessError>,
}
impl<O: Opener> Service<O> {
    pub(crate) fn new(opener: O, config: Config, now: Instant) -> Result<Self, Error> {
        config.validate(now)?;
        Ok(Self {
            opener,
            config,
            entries: BTreeMap::new(),
            cursor: 0,
            generation: 0,
            last_step: now,
            stopping: false,
            scope: crate::input::classification::CollectionScope::Keyboards,
            pending_admission: false,
        })
    }
    pub(crate) fn set_scope(&mut self, scope: crate::input::classification::CollectionScope) {
        self.scope = scope;
    }
    pub(crate) fn leases(&self) -> impl Iterator<Item = (InputEndpointId, u64, &O::Lease)> {
        self.entries.iter().filter_map(|(id, e)| {
            e.lease
                .as_ref()
                .filter(|_| !e.retiring)
                .map(|lease| (*id, e.generation, lease))
        })
    }
    pub(crate) fn status(&self, id: InputEndpointId) -> Option<(Option<HostHandle>, Option<Mode>)> {
        self.entries
            .get(&id)
            .map(|entry| (entry.handle, entry.lease.as_ref().map(Lease::mode)))
    }
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        // Re-admission after budgeted retirement must not wait for the next unrelated discovery scan.
        if self.pending_admission {
            return Some(self.last_step);
        }
        self.entries
            .values()
            .map(|entry| {
                if entry.retiring
                    || entry.pending.is_some()
                    || entry.failure.is_some()
                    || entry.drain_pending
                    || entry.ready
                {
                    return self.last_step;
                }
                entry
                    .recovery
                    .as_ref()
                    .map_or(entry.next, |r| r.until.min(entry.next))
            })
            .min()
    }
    pub(crate) fn step(
        &mut self,
        now: Instant,
        ready: &[Ready],
        runtime: &mut Runtime,
    ) -> Result<Outcome, Error> {
        if now < self.last_step || now > runtime.now() {
            return Err(
                RuntimeError::from(StateError::Clock("invalid indicator action time")).into(),
            );
        }
        self.config.validate(runtime.now())?;
        let mut outcome = Outcome {
            publications: vec![runtime.advance_time(now)?],
            problems: vec![],
        };
        self.last_step = now;
        if self.stopping {
            // A retried shutdown must finish retirement, never rediscover and reopen already closed sources.
            let mut stopped = self.stop(runtime)?;
            outcome.publications.append(&mut stopped.publications);
            self.last_step = runtime.now();
            return Ok(outcome);
        }
        let current: BTreeMap<_, _> = runtime
            .input_endpoints()
            .map(|e| (e.id, e.clone()))
            .collect();
        let admissions = crate::input::classification::admissions(current.values(), self.scope);
        for (id, entry) in &mut self.entries {
            if let Some(endpoint) = current.get(id) {
                entry.endpoint = endpoint.clone();
            }
            let keep = matches!(
                admissions.get(id),
                Some(crate::input::classification::Admission::Allow)
            ) || (matches!(
                admissions.get(id),
                Some(crate::input::classification::Admission::Uncertain)
            ) && entry.lease.is_some());
            if !keep {
                // A definite reclassification revokes admission even when the same input ID remains live.
                entry.retiring = true;
                entry.pending = None;
                entry.lease = None;
            }
        }
        for (id, endpoint) in current {
            if admissions[&id] == crate::input::classification::Admission::Allow {
                self.entries.entry(id).or_insert(Entry {
                    endpoint,
                    handle: None,
                    lease: None,
                    generation: 0,
                    sequence: 0,
                    pending: None,
                    failure: None,
                    unavailable: false,
                    next: now,
                    backoff: self.config.retry,
                    ready: false,
                    drain_pending: false,
                    recovery: None,
                    retiring: false,
                });
            }
        }
        for token in ready {
            if let Some(entry) = self.entries.get_mut(&token.endpoint)
                && entry.generation == token.generation
                && !entry.retiring
                && entry
                    .lease
                    .as_ref()
                    .is_some_and(|l| l.mode() == Mode::Events)
            {
                if token.closed {
                    entry.lease = None;
                    entry.pending = None;
                    entry.recovery = None;
                    entry.ready = false;
                    entry.drain_pending = false;
                    entry.failure = Some(HostUnavailable::Inaccessible);
                    entry.next = now + entry.backoff;
                    entry.backoff = entry.backoff.saturating_mul(2).min(self.config.max_retry);
                    outcome.problems.push(AccessError {
                        path: entry
                            .endpoint
                            .descriptor
                            .devnode
                            .known()
                            .cloned()
                            .unwrap_or_else(|| entry.endpoint.descriptor.key.0.clone()),
                        operation: "observe indicator endpoint closure",
                        kind: ErrorKind::Inaccessible,
                        source: None,
                    });
                } else {
                    entry.ready = true;
                }
            }
        }
        let ids: Vec<_> = self.entries.keys().copied().collect();
        let mut events = self.config.events;
        for offset in 0..ids.len().min(self.config.endpoints) {
            let id = ids[(self.cursor + offset) % ids.len()];
            // Temporarily own the entry so bookkeeping remains consistent even when a runtime action fails.
            let mut entry = self.entries.remove(&id).expect("listed entry");
            let result = self.process(&mut entry, &mut events, runtime, &mut outcome);
            let finished = entry.retiring && entry.handle.is_none();
            if !finished {
                self.entries.insert(id, entry);
            }
            if let Err(error) = result {
                self.last_step = runtime.now();
                return Err(error);
            }
        }
        if !ids.is_empty() {
            let visited = ids.len().min(self.config.endpoints);
            // A full endpoint round must still rotate the first claimant of the GLOBAL event budget.
            self.cursor =
                (self.cursor + if visited == ids.len() { 1 } else { visited }) % ids.len();
        }
        self.last_step = runtime.now();
        self.pending_admission = admissions.iter().any(|(id, admission)| {
            *admission == crate::input::classification::Admission::Allow
                && !self.entries.contains_key(id)
        });
        Ok(outcome)
    }
    fn process(
        &mut self,
        e: &mut Entry<O::Lease>,
        events: &mut usize,
        runtime: &mut Runtime,
        out: &mut Outcome,
    ) -> Result<(), Error> {
        if e.retiring {
            e.lease = None;
            if let Some(handle) = e.handle {
                out.publications
                    .push(runtime.retire_host(handle, runtime.now())?);
                e.handle = None;
            }
            return Ok(());
        }
        if e.handle.is_none() {
            let scope = NamespacedId::new("linux.evdev.indicators", e.endpoint.id.to_string())
                .map_err(RuntimeError::from)?;
            let (handle, publication) =
                runtime.register_host_for_input(scope, e.endpoint.id, runtime.now())?;
            e.handle = Some(handle);
            out.publications.push(publication);
        }
        self.publish_failure(e, runtime, out)?;
        if e.pending.is_some() {
            return self.publish(e, runtime, out);
        }
        let mut due = runtime.now() >= e.next;
        if e.lease.is_none() {
            if !due {
                return Ok(());
            }
            let generation = self.next_generation()?;
            match self.opener.open(&e.endpoint) {
                Ok(mut lease) => {
                    if let Some(warning) = lease.take_warning() {
                        out.problems.push(warning);
                    }
                    e.lease = Some(lease);
                    e.generation = generation;
                    due = true;
                }
                Err(error) => {
                    self.fail(e, error, runtime.now(), out);
                    self.publish_failure(e, runtime, out)?;
                    return Ok(());
                }
            }
        }
        if e.lease.as_ref().is_some_and(|l| l.mode() == Mode::Events)
            && (e.ready || e.drain_pending)
        {
            if *events == 0 {
                e.drain_pending = true;
                return Ok(());
            }
            let budget = self.config.events_per_endpoint.min(*events);
            e.ready = false;
            match e.lease.as_mut().expect("opened lease").drain(budget) {
                Ok(drain) => {
                    *events = events.saturating_sub(drain.processed.min(budget));
                    e.drain_pending = drain.pending;
                    due |= drain.changed;
                    if drain.gap {
                        e.pending = None;
                        e.failure = Some(HostUnavailable::EventGap);
                        if e.recovery.is_none() {
                            e.recovery = Some(Recovery {
                                until: runtime.now() + self.config.recovery,
                                synchronized: false,
                            });
                        }
                    }
                    if let Some(recovery) = e.recovery.as_mut() {
                        recovery.synchronized |= drain.synchronized;
                    }
                }
                Err(error) if error.kind == ErrorKind::Protocol => {
                    out.problems.push(error);
                    e.lease
                        .as_mut()
                        .expect("opened lease")
                        .downgrade(Fallback::StreamFailure);
                    e.failure = Some(HostUnavailable::EventGap);
                    e.recovery = None;
                    e.drain_pending = false;
                    due = true;
                }
                Err(error) => {
                    self.fail(e, error, runtime.now(), out);
                    self.publish_failure(e, runtime, out)?;
                    return Ok(());
                }
            }
            self.publish_failure(e, runtime, out)?;
        }
        if let Some(recovery) = &e.recovery {
            if !recovery.synchronized && runtime.now() < recovery.until {
                return Ok(());
            }
            if !recovery.synchronized {
                e.lease
                    .as_mut()
                    .expect("recovering lease")
                    .downgrade(Fallback::RecoveryTimeout);
                e.drain_pending = false;
            }
            e.recovery = None;
            due = true;
        }
        if !due {
            return Ok(());
        }
        let snapshot = match e.lease.as_mut().expect("opened lease").snapshot() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.fail(e, error, runtime.now(), out);
                self.publish_failure(e, runtime, out)?;
                return Ok(());
            }
        };
        let received = runtime.now();
        self.config.validate(received)?;
        e.sequence = if e.unavailable {
            1
        } else {
            e.sequence.checked_add(1).ok_or(Error::Exhausted)?
        };
        let interval = self
            .config
            .interval(e.lease.as_ref().expect("opened lease").mode());
        if e.lease.as_ref().expect("opened lease").mode() != Mode::Events {
            e.ready = false;
            e.drain_pending = false;
        }
        e.pending = Some(Receipt {
            value: snapshot,
            sequence: e.sequence,
            received,
            until: received + interval * 3,
        });
        e.next = received + interval;
        e.backoff = self.config.retry;
        self.publish(e, runtime, out)
    }
    fn next_generation(&mut self) -> Result<u64, Error> {
        self.generation = self.generation.checked_add(1).ok_or(Error::Exhausted)?;
        Ok(self.generation)
    }
    fn fail(&self, e: &mut Entry<O::Lease>, error: AccessError, now: Instant, out: &mut Outcome) {
        e.lease = None;
        e.pending = None;
        e.recovery = None;
        e.ready = false;
        e.drain_pending = false;
        e.failure = Some(error.unavailable());
        e.next = now + e.backoff;
        e.backoff = e.backoff.saturating_mul(2).min(self.config.max_retry);
        out.problems.push(error);
    }
    fn publish_failure(
        &self,
        e: &mut Entry<O::Lease>,
        runtime: &mut Runtime,
        out: &mut Outcome,
    ) -> Result<(), Error> {
        if let Some(reason) = e.failure {
            out.publications.push(runtime.host_unavailable(
                e.handle.expect("registered host"),
                reason,
                runtime.now(),
            )?);
            e.failure = None;
            e.unavailable = true;
        }
        Ok(())
    }
    fn publish(
        &mut self,
        e: &mut Entry<O::Lease>,
        runtime: &mut Runtime,
        out: &mut Outcome,
    ) -> Result<(), Error> {
        if e.pending.is_some() && e.unavailable {
            let generation = self.next_generation()?;
            let (handle, publication) =
                runtime.renew_host(e.handle.expect("registered host"), runtime.now())?;
            e.handle = Some(handle);
            e.unavailable = false;
            e.generation = generation;
            out.publications.push(publication);
        }
        if let Some(receipt) = &e.pending {
            out.publications.push(runtime.apply_host(
                e.handle.expect("registered host"),
                receipt.value.patch(),
                receipt.sequence,
                receipt.received,
                receipt.until,
                runtime.now(),
            )?);
            e.pending = None;
        }
        Ok(())
    }
    pub(crate) fn stop(&mut self, runtime: &mut Runtime) -> Result<Outcome, Error> {
        self.stopping = true;
        self.pending_admission = false;
        for e in self.entries.values_mut() {
            e.retiring = true;
            e.pending = None;
            e.lease = None;
        }
        let mut out = Outcome {
            publications: vec![],
            problems: vec![],
        };
        let ids: Vec<_> = self.entries.keys().copied().collect();
        for id in ids {
            let entry = self.entries.get_mut(&id).expect("listed entry");
            if let Some(handle) = entry.handle {
                out.publications
                    .push(runtime.retire_host(handle, runtime.now())?);
                entry.handle = None;
            }
            self.entries.remove(&id);
        }
        Ok(out)
    }
}
#[cfg(test)]
mod tests;
