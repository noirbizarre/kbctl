//! Immutable normalized records. Protocol objects and transport handles never enter snapshots.

use miette::Diagnostic;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Instant,
};
use thiserror::Error;

use super::{
    connection::{AttachmentToken, ConnectionState, OperationToken, TransportKind},
    identity::{
        BackendSessionId, ComponentId, EndpointId, HostGeneration, HostSourceId, InputDeviceId,
        InputEndpointId, InterfaceId, KeyboardId, MembershipGeneration, PhysicalDeviceId,
    },
    observation::{DeviceField, HostLock, MeasurementTime, NamespacedId, Observation, StatusValue},
    time::{IngressOrigin, Timestamp},
    topology::ComponentKind,
};

#[derive(Debug, Error, Diagnostic)]
pub(crate) enum StateError {
    #[error("normalized-state writer has closed")]
    #[diagnostic(
        code(kbctl::state::closed),
        help("Obtain a reader from a live runtime, then take a fresh snapshot and subscription.")
    )]
    Closed,
    #[error("invalid normalized-state input: {0}")]
    #[diagnostic(
        code(kbctl::state::invalid_input),
        help(
            "Correct the source, patch or capacity before retrying; absent fields must remain absent."
        )
    )]
    Invalid(&'static str),
    #[error("invalid monotonic state time: {0}")]
    #[diagnostic(
        code(kbctl::state::clock),
        help(
            "Use the runtime's host monotonic domain and nondecreasing action times; supply explicit freshness deadlines."
        )
    )]
    Clock(&'static str),
    #[error("could not generate a restart-safe state epoch")]
    #[diagnostic(
        code(kbctl::state::epoch),
        help("Restore OS entropy availability and retry before changing application state.")
    )]
    Epoch(#[source] std::io::Error),
    #[error("epoch generator repeated the current epoch")]
    #[diagnostic(
        code(kbctl::state::epoch_reused),
        help("Use a fresh opaque epoch; never reuse a cursor after revision exhaustion.")
    )]
    EpochReused,
    #[error(transparent)]
    #[diagnostic(transparent)]
    Model(#[from] super::ModelError),
}
pub(crate) type Result<T> = std::result::Result<T, StateError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Epoch([u8; 16]);
impl Epoch {
    pub(crate) fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Cursor {
    pub(crate) epoch: Epoch,
    pub(crate) revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeviceProvenance {
    pub(crate) reporter: String,
    pub(crate) session: BackendSessionId,
    pub(crate) attachment: AttachmentToken,
    pub(crate) operation: OperationToken,
}
impl DeviceProvenance {
    pub(crate) fn endpoint(&self) -> EndpointId {
        self.attachment.endpoint
    }
    pub(crate) fn component(&self) -> ComponentId {
        self.operation.component
    }
    pub(crate) fn keyboard(&self) -> KeyboardId {
        self.operation.keyboard
    }
    pub(crate) fn physical_source(&self) -> PhysicalDeviceId {
        self.attachment.physical
    }
    pub(crate) fn generation(&self) -> super::identity::AttachmentGeneration {
        self.attachment.generation
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Provenance {
    Device(DeviceProvenance),
    Host {
        source: HostSourceId,
        generation: HostGeneration,
        scope: NamespacedId,
    },
}
impl Provenance {
    pub(crate) fn same_generation(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Device(a), Self::Device(b)) => {
                a.session == b.session
                    && a.endpoint() == b.endpoint()
                    && a.component() == b.component()
            }
            (
                Self::Host {
                    source: a,
                    generation: ag,
                    ..
                },
                Self::Host {
                    source: b,
                    generation: bg,
                    ..
                },
            ) => a == b && ag == bg,
            _ => false,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReportOrigin {
    Device(IngressOrigin),
    Host,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReportStamp {
    pub(crate) source: Provenance,
    pub(crate) sequence: u64,
    pub(crate) received_at: Timestamp,
    pub(crate) fresh_until: Timestamp,
    pub(crate) measured_at: Option<MeasurementTime>,
    pub(crate) origin: ReportOrigin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceFailure {
    PermissionDenied,
    Busy,
    Suspended,
    Disconnected,
    Protocol,
    Timeout,
    Cancelled,
    Shutdown,
    InvalidBinding,
    EventGap,
    HostUnsupported,
    HostInaccessible,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StaleCause {
    Expired,
    Source(SourceFailure),
    Replaced,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Validity {
    Fresh,
    Stale { since: Timestamp, cause: StaleCause },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FieldQuality {
    Unknown,
    Fresh,
    Stale,
    Failed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FailureFact {
    pub(crate) at: Timestamp,
    pub(crate) cause: SourceFailure,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Report<T> {
    pub(crate) value: Observation<T>,
    pub(crate) stamp: ReportStamp,
    pub(crate) validity: Validity,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KnownSample<T> {
    pub(crate) value: T,
    pub(crate) stamp: ReportStamp,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FieldState<T> {
    pub(crate) report: Option<Report<T>>,
    pub(crate) last_known: Option<KnownSample<T>>,
    pub(crate) failure: Option<FailureFact>,
}
impl<T> Default for FieldState<T> {
    fn default() -> Self {
        Self {
            report: None,
            last_known: None,
            failure: None,
        }
    }
}
impl<T: Clone + Eq> FieldState<T> {
    pub(crate) fn quality(&self) -> FieldQuality {
        if self.failure.is_some() {
            return FieldQuality::Failed;
        }
        match &self.report {
            None => FieldQuality::Unknown,
            Some(Report {
                validity: Validity::Fresh,
                ..
            }) => FieldQuality::Fresh,
            Some(_) => FieldQuality::Stale,
        }
    }
    pub(crate) fn apply(
        &mut self,
        value: Observation<T>,
        stamp: ReportStamp,
        now: Timestamp,
    ) -> bool {
        if self.report.as_ref().is_some_and(|old| {
            old.stamp.source.same_generation(&stamp.source) && old.stamp.sequence >= stamp.sequence
        }) {
            return false;
        }
        if let Observation::Known(value) = &value {
            self.last_known = Some(KnownSample {
                value: value.clone(),
                stamp: stamp.clone(),
            });
        }
        let validity = if now >= stamp.fresh_until {
            Validity::Stale {
                since: stamp.fresh_until,
                cause: StaleCause::Expired,
            }
        } else {
            Validity::Fresh
        };
        self.report = Some(Report {
            value,
            stamp,
            validity,
        });
        self.failure = None;
        true
    }
    pub(crate) fn stale(&mut self, at: Timestamp, cause: StaleCause) {
        if let Some(report) = &mut self.report
            && report.validity == Validity::Fresh
        {
            report.validity = Validity::Stale { since: at, cause };
        }
    }
    pub(crate) fn fail(&mut self, at: Timestamp, cause: SourceFailure) {
        self.stale(at, StaleCause::Source(cause));
        self.failure = Some(FailureFact { at, cause });
    }
    pub(crate) fn expire(&mut self, now: Timestamp) {
        if let Some(report) = &mut self.report
            && report.validity == Validity::Fresh
            && now >= report.stamp.fresh_until
        {
            report.validity = Validity::Stale {
                since: report.stamp.fresh_until,
                cause: StaleCause::Expired,
            };
        }
    }
    pub(crate) fn next_deadline(&self) -> Option<Timestamp> {
        self.report
            .as_ref()
            .filter(|report| report.validity == Validity::Fresh)
            .map(|report| report.stamp.fresh_until)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceAvailability {
    Active,
    Unavailable(SourceFailure),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeviceState {
    pub(crate) source: DeviceProvenance,
    pub(crate) availability: SourceAvailability,
    pub(crate) continuity_lost: Option<(u64, Timestamp)>,
    pub(crate) fields: BTreeMap<DeviceField, FieldState<StatusValue>>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostState {
    pub(crate) id: HostSourceId,
    pub(crate) generation: HostGeneration,
    pub(crate) scope: NamespacedId,
    pub(crate) input: Option<InputEndpointId>,
    pub(crate) availability: SourceAvailability,
    pub(crate) fields: BTreeMap<HostLock, FieldState<bool>>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostValue {
    Known(bool),
    Unknown,
    Unavailable(HostUnavailable),
}
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct HostPatch(pub(crate) BTreeMap<HostLock, HostValue>);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostUnavailable {
    Unsupported,
    PermissionDenied,
    Inaccessible,
    EventGap,
}
impl HostUnavailable {
    pub(crate) fn failure(self) -> SourceFailure {
        match self {
            Self::Unsupported => SourceFailure::HostUnsupported,
            Self::PermissionDenied => SourceFailure::PermissionDenied,
            Self::Inaccessible => SourceFailure::HostInaccessible,
            Self::EventGap => SourceFailure::EventGap,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RecordKey {
    Keyboard(KeyboardId),
    Physical(PhysicalDeviceId),
    Interface(InterfaceId),
    Component(ComponentId),
    Endpoint(EndpointId),
    Session(BackendSessionId),
    Host(HostSourceId),
    Device(ComponentId, EndpointId),
    InputDevice(InputDeviceId),
    InputEndpoint(InputEndpointId),
    InputDiscovery,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StateRecord {
    Keyboard {
        id: KeyboardId,
        name: String,
    },
    Physical {
        id: PhysicalDeviceId,
        keyboard: KeyboardId,
        membership: MembershipGeneration,
        name: String,
    },
    Interface {
        id: InterfaceId,
        physical: PhysicalDeviceId,
        number: Option<u8>,
    },
    Component {
        id: ComponentId,
        physical: PhysicalDeviceId,
        keyboard: KeyboardId,
        parent: Option<ComponentId>,
        kind: ComponentKind,
        name: String,
    },
    Endpoint {
        id: EndpointId,
        physical: PhysicalDeviceId,
        interface: Option<InterfaceId>,
        transport: TransportKind,
        connection: ConnectionState,
        changed_at: Timestamp,
    },
    Session {
        id: BackendSessionId,
        reporter: String,
        endpoints: Vec<EndpointId>,
        availability: SourceAvailability,
        changed_at: Timestamp,
        continuity_lost: Option<(u64, Timestamp)>,
    },
    Host(Box<HostState>),
    Device(Box<DeviceState>),
    InputDevice(Box<super::input::InputDevice>),
    InputEndpoint(Box<super::input::InputEndpoint>),
    InputDiscovery(super::input::DiscoveryStatus),
}
impl StateRecord {
    pub(crate) fn key(&self) -> RecordKey {
        match self {
            Self::Keyboard { id, .. } => RecordKey::Keyboard(*id),
            Self::Physical { id, .. } => RecordKey::Physical(*id),
            Self::Interface { id, .. } => RecordKey::Interface(*id),
            Self::Component { id, .. } => RecordKey::Component(*id),
            Self::Endpoint { id, .. } => RecordKey::Endpoint(*id),
            Self::Session { id, .. } => RecordKey::Session(*id),
            Self::Host(state) => RecordKey::Host(state.id),
            Self::Device(state) => {
                RecordKey::Device(state.source.component(), state.source.endpoint())
            }
            Self::InputDevice(device) => RecordKey::InputDevice(device.id),
            Self::InputEndpoint(endpoint) => RecordKey::InputEndpoint(endpoint.id),
            Self::InputDiscovery(_) => RecordKey::InputDiscovery,
        }
    }
}
pub(crate) type Records = BTreeMap<RecordKey, Arc<StateRecord>>;
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub(crate) cursor: Cursor,
    pub(crate) time_domain: Epoch,
    pub(crate) records: Records,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Commit {
    pub(crate) cursor: Cursor,
    pub(crate) at: Timestamp,
    pub(crate) upserts: Records,
    pub(crate) removals: BTreeSet<RecordKey>,
}
#[derive(Debug, Clone, Default)]
pub(crate) struct CommitOutcome {
    pub(crate) commit: Option<Arc<Commit>>,
    pub(crate) ignored_stale_fields: usize,
}

/// Deadlines apply only to included leaves; omitted fields retain their own stamps.
#[derive(Debug, Clone)]
pub(crate) struct FreshnessDeadlines {
    default: Option<Instant>,
    fields: BTreeMap<DeviceField, Instant>,
}
impl FreshnessDeadlines {
    pub(crate) fn uniform(until: Instant) -> Self {
        Self {
            default: Some(until),
            fields: BTreeMap::new(),
        }
    }
    pub(crate) fn per_field(fields: BTreeMap<DeviceField, Instant>) -> Self {
        Self {
            default: None,
            fields,
        }
    }
    pub(crate) fn deadline(&self, field: &DeviceField) -> Result<Instant> {
        self.fields
            .get(field)
            .copied()
            .or(self.default)
            .ok_or(StateError::Invalid(
                "included leaf is missing a freshness deadline",
            ))
    }
}
