//! Exclusive application mutation seam. Readers only see complete published commits.
#![allow(dead_code)]

use miette::Diagnostic;
use std::{collections::BTreeMap, sync::Arc, time::Instant};
use thiserror::Error;

mod input;

use crate::{
    backends::{
        CapabilityDelivery, ObservationDelivery, PreparedRefresh, Session, SessionObserver,
        error::{BackendError, FailureKind},
        session::SessionState,
    },
    model::{
        self,
        capability::CapabilityScope,
        connection::{ConnectionState, UnavailableReason},
        identity::{
            BackendSessionId, ComponentId, EndpointId, HostGeneration, HostSourceId, KeyboardId,
            PhysicalDeviceId,
        },
        observation::{NamespacedId, Observation},
        state::*,
        time::{Clock, Timestamp},
        topology::{ComponentKind, DisplayName, PhysicalDeviceSpec, Topology},
    },
    state::{EpochSource, OsEpochs, Reservation, StateReader, StateStore},
};

#[derive(Debug, Error, Diagnostic)]
pub(crate) enum RuntimeError {
    #[error(transparent)]
    #[diagnostic(transparent)]
    State(#[from] StateError),
    #[error(transparent)]
    #[diagnostic(transparent)]
    Model(#[from] model::ModelError),
    #[error("backend observation source is no longer admissible")]
    #[diagnostic(
        code(kbctl::runtime::source_rejected),
        help(
            "Reconcile source lifecycle and topology, then establish a current observer; discard obsolete input."
        )
    )]
    Backend(#[from] BackendError),
    #[error(transparent)]
    #[diagnostic(transparent)]
    Input(#[from] input::InputError),
}
pub(crate) type Result<T> = std::result::Result<T, RuntimeError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HostHandle {
    id: HostSourceId,
    generation: HostGeneration,
}
impl HostHandle {
    pub(crate) fn id(self) -> HostSourceId {
        self.id
    }
}
struct Registered {
    observer: SessionObserver,
    availability: SourceAvailability,
    at: Timestamp,
    failed: bool,
    gap: Option<(u64, Timestamp)>,
}
#[derive(Clone, Copy)]
struct Life {
    availability: SourceAvailability,
    stopped_at: Option<Timestamp>,
    failed: bool,
    gap: Option<(u64, Timestamp)>,
}
struct Action {
    at: Timestamp,
    cursor: Reservation,
    lives: BTreeMap<BackendSessionId, Life>,
}

pub(crate) struct Runtime {
    inputs: input::Inventory,
    topology: Topology,
    store: StateStore,
    clock: Arc<dyn Clock>,
    origin: Instant,
    last_action: Timestamp,
    devices: BTreeMap<(ComponentId, EndpointId), DeviceState>,
    hosts: BTreeMap<HostSourceId, HostState>,
    sessions: BTreeMap<BackendSessionId, Registered>,
    owners: BTreeMap<EndpointId, BackendSessionId>,
    host_frontiers: BTreeMap<HostSourceId, (u64, Timestamp)>,
    endpoint_times: BTreeMap<EndpointId, (ConnectionState, Timestamp)>,
}

impl Runtime {
    pub(crate) fn new(topology: Topology, clock: Arc<dyn Clock>) -> Result<Self> {
        Self::with_epochs(topology, clock, Box::new(OsEpochs))
    }
    pub(crate) fn with_epochs(
        topology: Topology,
        clock: Arc<dyn Clock>,
        epochs: Box<dyn EpochSource>,
    ) -> Result<Self> {
        let origin = clock.now();
        let mut endpoint_times = BTreeMap::new();
        let records = topology_records(&topology, &mut endpoint_times, Timestamp::ZERO);
        Ok(Self {
            inputs: input::Inventory::default(),
            topology,
            store: StateStore::new(records, epochs)?,
            clock,
            origin,
            last_action: Timestamp::ZERO,
            devices: BTreeMap::new(),
            hosts: BTreeMap::new(),
            sessions: BTreeMap::new(),
            owners: BTreeMap::new(),
            host_frontiers: BTreeMap::new(),
            endpoint_times,
        })
    }
    pub(crate) fn topology(&self) -> &Topology {
        &self.topology
    }
    pub(crate) fn reader(&self) -> StateReader {
        self.store.reader()
    }
    pub(crate) fn now(&self) -> Instant {
        self.clock.now()
    }
    fn timestamp(&self, instant: Instant) -> std::result::Result<Timestamp, StateError> {
        instant
            .checked_duration_since(self.origin)
            .map(Timestamp)
            .ok_or(StateError::Clock(
                "instant predates the runtime time domain",
            ))
    }
    fn life(
        &self,
        observer: &SessionObserver,
        at: Timestamp,
    ) -> std::result::Result<Life, StateError> {
        let life = observer.life();
        let stopped_at = life
            .stopped_at
            .map(|time| self.timestamp(time))
            .transpose()?;
        let gap = life
            .gap
            .map(|stamp| {
                self.timestamp(stamp.received_at)
                    .map(|time| (stamp.sequence, time))
            })
            .transpose()?;
        if stopped_at.is_some_and(|time| time > at) || gap.is_some_and(|(_, time)| time > at) {
            return Err(StateError::Clock(
                "lifecycle fact is later than this action",
            ));
        }
        let availability = if life.state == SessionState::Active {
            SourceAvailability::Active
        } else {
            SourceAvailability::Unavailable(life.failure.map(map_failure).unwrap_or(
                match life.state {
                    SessionState::Shutdown => SourceFailure::Shutdown,
                    _ => SourceFailure::Disconnected,
                },
            ))
        };
        Ok(Life {
            availability,
            stopped_at,
            failed: life.state == SessionState::Failed,
            gap,
        })
    }
    fn prepare(&mut self, now: Instant) -> Result<Action> {
        if now > self.clock.now() {
            return Err(StateError::Clock("action timestamp is in the future").into());
        }
        let at = self.timestamp(now)?;
        if at < self.last_action {
            return Err(StateError::Clock("action time regressed").into());
        }
        // Snapshot external liveness once. No later fallible timestamp conversion
        // may occur after a topology mutation has become visible to this writer.
        let lives = self
            .sessions
            .iter()
            .map(|(id, session)| self.life(&session.observer, at).map(|life| (*id, life)))
            .collect::<std::result::Result<_, _>>()?;
        Ok(Action {
            at,
            cursor: self.store.reserve()?,
            lives,
        })
    }
    fn finish(&mut self, action: Action, ignored_stale_fields: usize) -> CommitOutcome {
        self.reconcile(&action.lives, action.at);
        // Input retirement must invalidate evidence in the same commit, before a collector runs again.
        let inputs: std::collections::BTreeSet<_> =
            self.input_endpoints().map(|endpoint| endpoint.id).collect();
        for host in self.hosts.values_mut() {
            if host.input.is_some_and(|id| !inputs.contains(&id)) {
                host.availability = SourceAvailability::Unavailable(SourceFailure::InvalidBinding);
                for field in host.fields.values_mut() {
                    field.stale(action.at, StaleCause::Source(SourceFailure::InvalidBinding));
                }
            }
        }
        let mut records = topology_records(&self.topology, &mut self.endpoint_times, action.at);
        self.inputs.project(&self.topology, &mut records);
        for (id, registered) in &self.sessions {
            let record = StateRecord::Session {
                id: *id,
                reporter: registered.observer.reporter().into(),
                endpoints: registered.observer.endpoints().collect(),
                availability: registered.availability,
                changed_at: registered.at,
                continuity_lost: registered.gap,
            };
            records.insert(record.key(), Arc::new(record));
        }
        for host in self.hosts.values() {
            let record = StateRecord::Host(Box::new(host.clone()));
            records.insert(record.key(), Arc::new(record));
        }
        for device in self.devices.values() {
            let record = StateRecord::Device(Box::new(device.clone()));
            records.insert(record.key(), Arc::new(record));
        }
        self.last_action = action.at;
        CommitOutcome {
            commit: self.store.publish(action.cursor, records, action.at),
            ignored_stale_fields,
        }
    }
    fn reconcile(&mut self, lives: &BTreeMap<BackendSessionId, Life>, now: Timestamp) {
        self.owners
            .retain(|endpoint, _| self.topology.endpoint(*endpoint).is_ok());
        self.devices.retain(|(component, endpoint), _| {
            self.topology.component(*component).is_ok() && self.topology.endpoint(*endpoint).is_ok()
        });
        for (id, registered) in &mut self.sessions {
            let life = lives
                .get(id)
                .expect("registered source lifecycle was prepared");
            let mut availability = life.availability;
            if availability == SourceAvailability::Active
                && registered
                    .observer
                    .validate_binding(&self.topology)
                    .is_err()
            {
                let failure = registered
                    .observer
                    .endpoints()
                    .find_map(|endpoint| {
                        self.topology.endpoint(endpoint).ok().and_then(|endpoint| {
                            match endpoint.state() {
                                ConnectionState::Unavailable(reason) => {
                                    Some(map_unavailable(reason))
                                }
                                _ => None,
                            }
                        })
                    })
                    .unwrap_or(SourceFailure::InvalidBinding);
                availability = SourceAvailability::Unavailable(failure);
            }
            if registered.availability != availability || registered.failed != life.failed {
                registered.at = life.stopped_at.unwrap_or(now);
                registered.availability = availability;
                registered.failed = life.failed;
            }
            registered.gap = life.gap;
            for device in self
                .devices
                .values_mut()
                .filter(|device| device.source.session == *id)
            {
                device.availability = availability;
                device.continuity_lost = life.gap;
                for field in device.fields.values_mut() {
                    if let SourceAvailability::Unavailable(cause) = availability {
                        if life.failed {
                            field.fail(registered.at, cause);
                        } else {
                            field.stale(registered.at, StaleCause::Source(cause));
                        }
                    }
                    if let Some((sequence, at)) = life.gap
                        && field.report.as_ref().is_some_and(|report| {
                            report
                                .stamp
                                .source
                                .same_generation(&Provenance::Device(device.source.clone()))
                                && report.stamp.sequence <= sequence
                        })
                    {
                        field.stale(at, StaleCause::Source(SourceFailure::EventGap));
                    }
                }
            }
        }
        // Keep only current route generations. Reconnect never creates a session
        // history, and retirement cannot leave orphaned status or ordering records.
        self.sessions.retain(|id, _| {
            self.owners.values().any(|owner| owner == id)
                || self
                    .devices
                    .values()
                    .any(|device| device.source.session == *id)
        });
    }

    pub(crate) fn register_session(
        &mut self,
        observer: SessionObserver,
        now: Instant,
    ) -> Result<CommitOutcome> {
        observer.validate(&self.topology)?;
        for endpoint in observer.endpoints() {
            if let Some(id) = self.owners.get(&endpoint)
                && *id != observer.id()
                && self.sessions[id].observer.life().state == SessionState::Active
            {
                return Err(StateError::Invalid(
                    "stop the old endpoint owner before registering a replacement",
                )
                .into());
            }
        }
        let mut action = self.prepare(now)?;
        let life = self.life(&observer, action.at)?;
        action.lives.insert(observer.id(), life);
        let sources = observer.sources();
        let endpoints: Vec<_> = observer.endpoints().collect();
        self.devices.retain(|key, _| {
            !endpoints.contains(&key.1)
                || sources
                    .iter()
                    .any(|source| (source.component(), source.endpoint()) == *key)
        });
        for source in sources {
            let key = (source.component(), source.endpoint());
            match self.devices.entry(key) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(DeviceState {
                        source,
                        availability: SourceAvailability::Active,
                        continuity_lost: None,
                        fields: BTreeMap::new(),
                    });
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    let device = entry.get_mut();
                    if device.source.session != source.session {
                        for field in device.fields.values_mut() {
                            field.stale(action.at, StaleCause::Replaced);
                            field.failure = None;
                        }
                        device.source = source;
                        device.availability = SourceAvailability::Active;
                        device.continuity_lost = None;
                    }
                }
            }
        }
        for endpoint in endpoints {
            self.owners.insert(endpoint, observer.id());
        }
        self.sessions.entry(observer.id()).or_insert(Registered {
            observer,
            availability: life.availability,
            at: action.at,
            failed: life.failed,
            gap: life.gap,
        });
        Ok(self.finish(action, 0))
    }

    pub(crate) fn accept_observation(
        &mut self,
        observer: &SessionObserver,
        delivery: &ObservationDelivery,
        deadlines: &FreshnessDeadlines,
        now: Instant,
    ) -> Result<CommitOutcome> {
        let value = observer.accept(&self.topology, delivery)?;
        let patch = value.patch();
        let key = (delivery.component(), delivery.source());
        let device = self.devices.get(&key).ok_or(StateError::Invalid(
            "register the source before accepting observations",
        ))?;
        if device.source.session != observer.id()
            || self.owners.get(&delivery.source()) != Some(&observer.id())
        {
            return Err(
                StateError::Invalid("delivery is from a replaced source generation").into(),
            );
        }
        let at = self.timestamp(now)?;
        let ingress = delivery.ingress();
        let received_at = self.timestamp(ingress.received_at)?;
        if ingress.sequence == 0 || received_at > at {
            return Err(StateError::Clock("invalid source receipt timestamp/sequence").into());
        }
        let mut stamps = BTreeMap::new();
        for (field, leaf) in patch.fields() {
            if let Observation::Known(value) = &leaf.value
                && !field.accepts(value)
            {
                return Err(StateError::Invalid("status field/value type mismatch").into());
            }
            let fresh_until = self.timestamp(deadlines.deadline(field)?)?;
            if fresh_until < received_at {
                return Err(StateError::Clock("freshness deadline precedes receipt").into());
            }
            stamps.insert(
                field.clone(),
                ReportStamp {
                    source: Provenance::Device(device.source.clone()),
                    sequence: ingress.sequence,
                    received_at,
                    fresh_until,
                    measured_at: leaf.measured_at.clone(),
                    origin: ReportOrigin::Device(ingress.origin),
                },
            );
        }
        let action = self.prepare(now)?;
        let device = self.devices.get_mut(&key).expect("validated source");
        let mut ignored = 0;
        for (field, leaf) in patch.fields() {
            if !device.fields.entry(field.clone()).or_default().apply(
                leaf.value.clone(),
                stamps.remove(field).expect("validated deadline"),
                action.at,
            ) {
                ignored += 1;
            }
        }
        Ok(self.finish(action, ignored))
    }

    pub(crate) fn synchronize(&mut self, now: Instant) -> Result<CommitOutcome> {
        let action = self.prepare(now)?;
        Ok(self.finish(action, 0))
    }
    pub(crate) fn advance_time(&mut self, now: Instant) -> Result<CommitOutcome> {
        let action = self.prepare(now)?;
        for device in self.devices.values_mut() {
            for field in device.fields.values_mut() {
                field.expire(action.at);
            }
        }
        for host in self.hosts.values_mut() {
            for field in host.fields.values_mut() {
                field.expire(action.at);
            }
        }
        Ok(self.finish(action, 0))
    }
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.devices
            .values()
            .flat_map(|device| device.fields.values().filter_map(FieldState::next_deadline))
            .chain(
                self.hosts
                    .values()
                    .flat_map(|host| host.fields.values().filter_map(FieldState::next_deadline)),
            )
            .min()
            .map(|time| {
                self.origin
                    .checked_add(time.0)
                    .expect("deadline originated from a valid Instant")
            })
    }

    pub(crate) fn register_host(
        &mut self,
        scope: NamespacedId,
        now: Instant,
    ) -> Result<(HostHandle, CommitOutcome)> {
        self.register_host_binding(scope, None, now)
    }
    pub(crate) fn register_host_for_input(
        &mut self,
        scope: NamespacedId,
        input: crate::model::identity::InputEndpointId,
        now: Instant,
    ) -> Result<(HostHandle, CommitOutcome)> {
        self.register_host_binding(scope, Some(input), now)
    }
    fn register_host_binding(
        &mut self,
        scope: NamespacedId,
        input: Option<crate::model::identity::InputEndpointId>,
        now: Instant,
    ) -> Result<(HostHandle, CommitOutcome)> {
        self.validate_host_input(input)?;
        let action = self.prepare(now)?;
        let id = HostSourceId::new()?;
        let generation = HostGeneration::new()?;
        self.hosts.insert(
            id,
            HostState {
                id,
                generation,
                scope,
                input,
                availability: SourceAvailability::Active,
                fields: BTreeMap::new(),
            },
        );
        Ok((HostHandle { id, generation }, self.finish(action, 0)))
    }
    fn validate_host_input(
        &self,
        input: Option<crate::model::identity::InputEndpointId>,
    ) -> std::result::Result<(), StateError> {
        if input.is_some_and(|id| self.input_endpoint(id).is_none()) {
            return Err(StateError::Invalid(
                "host source input binding is retired; register a source for the replacement endpoint",
            ));
        }
        Ok(())
    }
    fn host(&self, handle: HostHandle) -> std::result::Result<&HostState, StateError> {
        self.hosts
            .get(&handle.id)
            .filter(|host| host.generation == handle.generation)
            .ok_or(StateError::Invalid(
                "host source handle is retired or superseded",
            ))
    }
    pub(crate) fn renew_host(
        &mut self,
        handle: HostHandle,
        now: Instant,
    ) -> Result<(HostHandle, CommitOutcome)> {
        self.validate_host_input(self.host(handle)?.input)?;
        let action = self.prepare(now)?;
        let generation = HostGeneration::new()?;
        let host = self.hosts.get_mut(&handle.id).expect("validated host");
        for field in host.fields.values_mut() {
            field.stale(action.at, StaleCause::Replaced);
            field.failure = None;
        }
        host.generation = generation;
        host.availability = SourceAvailability::Active;
        self.host_frontiers.remove(&handle.id);
        Ok((
            HostHandle {
                id: handle.id,
                generation,
            },
            self.finish(action, 0),
        ))
    }
    pub(crate) fn retire_host(
        &mut self,
        handle: HostHandle,
        now: Instant,
    ) -> Result<CommitOutcome> {
        self.host(handle)?;
        let action = self.prepare(now)?;
        self.hosts.remove(&handle.id);
        self.host_frontiers.remove(&handle.id);
        Ok(self.finish(action, 0))
    }
    pub(crate) fn host_unavailable(
        &mut self,
        handle: HostHandle,
        reason: HostUnavailable,
        now: Instant,
    ) -> Result<CommitOutcome> {
        self.host(handle)?;
        let action = self.prepare(now)?;
        let host = self.hosts.get_mut(&handle.id).expect("validated host");
        host.availability = SourceAvailability::Unavailable(reason.failure());
        for field in host.fields.values_mut() {
            field.stale(action.at, StaleCause::Source(reason.failure()));
        }
        Ok(self.finish(action, 0))
    }
    pub(crate) fn apply_host(
        &mut self,
        handle: HostHandle,
        patch: HostPatch,
        sequence: u64,
        received: Instant,
        until: Instant,
        now: Instant,
    ) -> Result<CommitOutcome> {
        let host = self.host(handle)?;
        self.validate_host_input(host.input)?;
        if host.availability != SourceAvailability::Active {
            return Err(StateError::Invalid(
                "renew unavailable host source before accepting reports",
            )
            .into());
        }
        let received_at = self.timestamp(received)?;
        let fresh_until = self.timestamp(until)?;
        let at = self.timestamp(now)?;
        if sequence == 0 || received_at > at || fresh_until < received_at {
            return Err(StateError::Clock("invalid host receipt/deadline").into());
        }
        if let Some((last, time)) = self.host_frontiers.get(&handle.id)
            && ((sequence > *last && received_at < *time)
                || (sequence < *last && received_at > *time)
                || (sequence == *last && received_at != *time))
        {
            return Err(
                StateError::Clock("host ingress clock disagrees with source sequence").into(),
            );
        }
        for field in host.fields.values() {
            let stamps = field
                .report
                .iter()
                .map(|report| &report.stamp)
                .chain(field.last_known.iter().map(|sample| &sample.stamp));
            for stamp in stamps {
                if matches!(stamp.source, Provenance::Host { generation, .. } if generation == handle.generation)
                    && !receipt_order(sequence, received_at, stamp.sequence, stamp.received_at)
                {
                    return Err(StateError::Clock(
                        "host receipt contradicts retained per-field ingress evidence",
                    )
                    .into());
                }
            }
        }
        let source = Provenance::Host {
            source: handle.id,
            generation: handle.generation,
            scope: host.scope.clone(),
        };
        let action = self.prepare(now)?;
        let host = self.hosts.get_mut(&handle.id).expect("validated host");
        let mut ignored = 0;
        for (field, value) in patch.0 {
            let failure = match value {
                HostValue::Unavailable(reason) => Some(reason.failure()),
                _ => None,
            };
            let value = match value {
                HostValue::Known(value) => Observation::Known(value),
                HostValue::Unknown => Observation::Unknown,
                HostValue::Unavailable(_) => Observation::Unknown,
            };
            let stamp = ReportStamp {
                source: source.clone(),
                sequence,
                received_at,
                fresh_until,
                measured_at: None,
                origin: ReportOrigin::Host,
            };
            let field = host.fields.entry(field).or_default();
            if !field.apply(value, stamp, action.at) {
                ignored += 1;
            } else if let Some(cause) = failure {
                // A rejected unavailable receipt must never poison newer accepted knowledge.
                // Its failure time belongs to that receipt, not to a possibly delayed publication retry.
                field.fail(received_at, cause);
            }
        }
        if self
            .host_frontiers
            .get(&handle.id)
            .is_none_or(|(last, _)| sequence > *last)
        {
            self.host_frontiers
                .insert(handle.id, (sequence, received_at));
        }
        Ok(self.finish(action, ignored))
    }

    pub(crate) fn add_physical_device(
        &mut self,
        keyboard: Option<KeyboardId>,
        spec: PhysicalDeviceSpec,
        now: Instant,
    ) -> Result<(PhysicalDeviceId, CommitOutcome)> {
        let action = self.prepare(now)?;
        let id = self.topology.add_physical_device(keyboard, spec)?;
        Ok((id, self.finish(action, 0)))
    }
    pub(crate) fn add_component(
        &mut self,
        physical: PhysicalDeviceId,
        parent: Option<ComponentId>,
        kind: ComponentKind,
        name: Option<DisplayName>,
        now: Instant,
    ) -> Result<(ComponentId, CommitOutcome)> {
        let action = self.prepare(now)?;
        let id = self.topology.add_component(physical, parent, kind, name)?;
        Ok((id, self.finish(action, 0)))
    }
    pub(crate) fn remove_component(
        &mut self,
        id: ComponentId,
        now: Instant,
    ) -> Result<CommitOutcome> {
        let action = self.prepare(now)?;
        self.topology.remove_component(id)?;
        Ok(self.finish(action, 0))
    }
    pub(crate) fn retire_physical_device(
        &mut self,
        id: PhysicalDeviceId,
        now: Instant,
    ) -> Result<CommitOutcome> {
        let action = self.prepare(now)?;
        self.topology.retire_physical_device(id)?;
        Ok(self.finish(action, 0))
    }
    pub(crate) fn associate_physical_device(
        &mut self,
        id: PhysicalDeviceId,
        keyboard: KeyboardId,
        now: Instant,
    ) -> Result<CommitOutcome> {
        let action = self.prepare(now)?;
        self.topology.associate_physical_device(id, keyboard)?;
        Ok(self.finish(action, 0))
    }
    pub(crate) fn rename_keyboard(
        &mut self,
        id: KeyboardId,
        name: DisplayName,
        now: Instant,
    ) -> Result<CommitOutcome> {
        let action = self.prepare(now)?;
        self.topology.rename_keyboard(id, name)?;
        Ok(self.finish(action, 0))
    }
    pub(crate) fn rename_physical_device(
        &mut self,
        id: PhysicalDeviceId,
        name: DisplayName,
        now: Instant,
    ) -> Result<CommitOutcome> {
        let action = self.prepare(now)?;
        self.topology.rename_physical_device(id, name)?;
        Ok(self.finish(action, 0))
    }
    pub(crate) fn revalidate_endpoint(
        &mut self,
        id: EndpointId,
        now: Instant,
    ) -> Result<CommitOutcome> {
        let action = self.prepare(now)?;
        self.topology.revalidate_endpoint(id)?;
        Ok(self.finish(action, 0))
    }
    pub(crate) fn invalidate_endpoint(
        &mut self,
        id: EndpointId,
        reason: UnavailableReason,
        now: Instant,
    ) -> Result<CommitOutcome> {
        let action = self.prepare(now)?;
        self.topology.invalidate_endpoint(id, reason)?;
        Ok(self.finish(action, 0))
    }

    // Capability bookkeeping is orthogonal to observed values: these operations
    // intentionally cannot renew any normalized field's timestamp or freshness.
    pub(crate) fn prepare_capabilities(
        &mut self,
        session: &Session,
        scope: CapabilityScope,
    ) -> Result<PreparedRefresh> {
        Ok(session.prepare_refresh(&mut self.topology, scope)?)
    }
    pub(crate) fn accept_capabilities(
        &mut self,
        session: &Session,
        prepared: PreparedRefresh,
        delivery: CapabilityDelivery,
    ) -> Result<()> {
        Ok(session.accept_capabilities(&mut self.topology, prepared, delivery)?)
    }
    pub(crate) fn abandon_capabilities(&mut self, prepared: PreparedRefresh) -> Result<()> {
        Ok(prepared.abandon(&mut self.topology)?)
    }
    pub(crate) fn invalidate_capabilities(&mut self, keyboard: KeyboardId) -> Result<()> {
        Ok(self.topology.invalidate_capabilities(keyboard)?)
    }
}

fn receipt_order(sequence: u64, time: Timestamp, previous: u64, previous_time: Timestamp) -> bool {
    match sequence.cmp(&previous) {
        std::cmp::Ordering::Greater => time >= previous_time,
        std::cmp::Ordering::Less => time <= previous_time,
        std::cmp::Ordering::Equal => time == previous_time,
    }
}
fn map_failure(kind: FailureKind) -> SourceFailure {
    match kind {
        FailureKind::AccessDenied => SourceFailure::PermissionDenied,
        FailureKind::Busy | FailureKind::ExclusiveAccess => SourceFailure::Busy,
        FailureKind::DeadlineExceeded => SourceFailure::Timeout,
        FailureKind::Cancelled => SourceFailure::Cancelled,
        FailureKind::Disconnected | FailureKind::Closed => SourceFailure::Disconnected,
        FailureKind::ResyncRequired => SourceFailure::EventGap,
        _ => SourceFailure::Protocol,
    }
}
fn map_unavailable(reason: UnavailableReason) -> SourceFailure {
    match reason {
        UnavailableReason::PermissionDenied => SourceFailure::PermissionDenied,
        UnavailableReason::Busy => SourceFailure::Busy,
        UnavailableReason::Suspended => SourceFailure::Suspended,
        UnavailableReason::Disconnected => SourceFailure::Disconnected,
        UnavailableReason::ProtocolFailure => SourceFailure::Protocol,
    }
}
fn topology_records(
    topology: &Topology,
    endpoint_times: &mut BTreeMap<EndpointId, (ConnectionState, Timestamp)>,
    now: Timestamp,
) -> Records {
    let mut records = Records::new();
    let mut insert = |record: StateRecord| {
        records.insert(record.key(), Arc::new(record));
    };
    for keyboard in topology.keyboards() {
        insert(StateRecord::Keyboard {
            id: keyboard.id(),
            name: keyboard.name().as_str().into(),
        });
    }
    for physical in topology.physical_devices() {
        insert(StateRecord::Physical {
            id: physical.id(),
            keyboard: physical.keyboard(),
            membership: physical.membership(),
            name: physical.name().as_str().into(),
        });
        for interface in topology.interfaces(physical.id()) {
            insert(StateRecord::Interface {
                id: interface.id(),
                physical: physical.id(),
                number: interface.usb_number(),
            });
        }
        for component in topology.components(physical.id()) {
            insert(StateRecord::Component {
                id: component.id(),
                physical: physical.id(),
                keyboard: component.keyboard(),
                parent: component.parent(),
                kind: component.kind(),
                name: component.name().as_str().into(),
            });
        }
        for endpoint in topology.endpoints(physical.id()) {
            let time = endpoint_times
                .entry(endpoint.id())
                .or_insert((endpoint.state(), now));
            if time.0 != endpoint.state() {
                *time = (endpoint.state(), now);
            }
            insert(StateRecord::Endpoint {
                id: endpoint.id(),
                physical: physical.id(),
                interface: endpoint.interface(),
                transport: endpoint.transport(),
                connection: endpoint.state(),
                changed_at: time.1,
            });
        }
    }
    endpoint_times.retain(|id, _| topology.endpoint(*id).is_ok());
    records
}

#[cfg(test)]
mod tests;
