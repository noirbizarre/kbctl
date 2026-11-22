//! Runtime-neutral protocol contracts. Vendor mappings and OS adapters are separate.

// Contracts are compiled and exercised now; the application host is still pending.
#![allow(dead_code)]

mod binding;
pub(crate) mod error;
pub(crate) mod events;
mod observer;
pub(crate) mod session;

use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::{Duration, Instant},
};

use crate::{
    model::{
        capability::{CapabilityReport, CapabilityScope},
        identity::{AdvertisedSerial, BackendSessionId, ComponentId, EndpointId, PhysicalDeviceId},
        observation::DeviceObservation,
        topology::{Endpoint, Interface, PhysicalDevice, Topology, UsbIds},
    },
    transport::LeaseSet,
};

pub(crate) use crate::model::time::Clock;
pub(crate) use binding::ConnectContext;
use error::{BackendError, FailureKind, Result};
pub(crate) use observer::SessionObserver;
pub(crate) use session::Session;

pub(crate) type BackendFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UpdateStrategy {
    Polling { interval: Duration },
    Push,
    Hybrid { interval: Duration },
}

#[derive(Debug, Clone)]
pub(crate) struct BackendDescriptor {
    id: String,
    updates: UpdateStrategy,
}
impl BackendDescriptor {
    pub(crate) fn new(id: impl Into<String>, updates: UpdateStrategy) -> Result<Self> {
        let id = id.into();
        if id.trim().is_empty()
            || matches!(updates, UpdateStrategy::Polling { interval } | UpdateStrategy::Hybrid { interval } if interval.is_zero())
        {
            return Err(BackendError::new(
                FailureKind::InvalidInput,
                "backend identity and polling interval must be nonempty/nonzero",
            ));
        }
        Ok(Self { id, updates })
    }
    pub(crate) fn id(&self) -> &str {
        &self.id
    }
    pub(crate) fn updates(&self) -> UpdateStrategy {
        self.updates
    }
}

pub(crate) struct DiscoveryView<'a> {
    physical: &'a PhysicalDevice,
    interfaces: Vec<&'a Interface>,
    endpoints: Vec<&'a Endpoint>,
}
impl<'a> DiscoveryView<'a> {
    pub(crate) fn new(topology: &'a Topology, physical: PhysicalDeviceId) -> Result<Self> {
        Ok(Self {
            physical: topology.physical_device(physical).map_err(|e| {
                BackendError::new(FailureKind::InvalidInput, "unknown discovery unit").caused_by(e)
            })?,
            interfaces: topology.interfaces(physical).collect(),
            endpoints: topology.endpoints(physical).collect(),
        })
    }
    pub(crate) fn physical(&self) -> &'a PhysicalDevice {
        self.physical
    }
    pub(crate) fn interfaces(&self) -> impl Iterator<Item = &'a Interface> + '_ {
        self.interfaces.iter().copied()
    }
    pub(crate) fn endpoints(&self) -> impl Iterator<Item = &'a Endpoint> + '_ {
        self.endpoints.iter().copied()
    }

    /// A protocol matcher sees only the channels justified by a registry route,
    /// never descriptors borrowed from a foreign physical device or sibling route.
    pub(crate) fn restricted(&self, selected: &[EndpointId]) -> Result<Self> {
        let unique: BTreeSet<_> = selected.iter().copied().collect();
        if unique.len() != selected.len()
            || !unique
                .iter()
                .all(|id| self.endpoints().any(|endpoint| endpoint.id() == *id))
        {
            return Err(BackendError::new(
                FailureKind::InvalidInput,
                "restricted discovery selection contains duplicate or foreign endpoints",
            ));
        }
        let endpoints: Vec<_> = self
            .endpoints()
            .filter(|endpoint| unique.contains(&endpoint.id()))
            .collect();
        let interface_ids: BTreeSet<_> = endpoints
            .iter()
            .filter_map(|endpoint| endpoint.interface())
            .collect();
        let interfaces = self
            .interfaces()
            .filter(|interface| interface_ids.contains(&interface.id()))
            .collect();
        Ok(Self {
            physical: self.physical,
            interfaces,
            endpoints,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum MatchEvidence {
    UsbModel(UsbIds),
    Interface(u8),
    Transport(crate::model::connection::TransportKind),
    AdvertisedSerial(AdvertisedSerial),
    HidUsagePage(u16),
    HidUsage(u16),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BackendCandidate {
    backend: String,
    configuration: String,
    endpoints: Vec<EndpointId>,
    evidence: Vec<MatchEvidence>,
}
impl BackendCandidate {
    pub(crate) fn new(
        backend: &str,
        configuration: &str,
        mut endpoints: Vec<EndpointId>,
        mut evidence: Vec<MatchEvidence>,
    ) -> Result<Self> {
        let unique: std::collections::BTreeSet<_> = endpoints.iter().collect();
        if backend.trim().is_empty()
            || configuration.trim().is_empty()
            || endpoints.is_empty()
            || unique.len() != endpoints.len()
            || evidence.is_empty()
        {
            return Err(BackendError::new(
                FailureKind::InvalidInput,
                "candidate needs a backend/configuration, distinct endpoints and match evidence",
            ));
        }
        // Endpoints describe an ownership set. Protocol roles and the primary
        // source must be carried explicitly, never encoded in vector order.
        endpoints.sort();
        evidence.sort();
        evidence.dedup();
        Ok(Self {
            backend: backend.into(),
            configuration: configuration.into(),
            endpoints,
            evidence,
        })
    }
    pub(crate) fn backend(&self) -> &str {
        &self.backend
    }
    pub(crate) fn configuration(&self) -> &str {
        &self.configuration
    }
    pub(crate) fn endpoints(&self) -> &[EndpointId] {
        &self.endpoints
    }
    pub(crate) fn evidence(&self) -> &[MatchEvidence] {
        &self.evidence
    }
    pub(crate) fn add_evidence(&mut self, evidence: impl IntoIterator<Item = MatchEvidence>) {
        self.evidence.extend(evidence);
        self.evidence.sort();
        self.evidence.dedup();
    }
}

#[derive(Debug)]
pub(crate) enum MatchHint {
    NoMatch,
    Candidate(BackendCandidate),
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MatchResolution {
    NoMatch,
    Candidate(BackendCandidate),
    Ambiguous(Vec<BackendCandidate>),
}
impl MatchResolution {
    /// Preserve ambiguity instead of smuggling registry ranking into this contract.
    pub(crate) fn from_hints(hints: impl IntoIterator<Item = MatchHint>) -> Self {
        let mut canonical: BTreeMap<_, BackendCandidate> = BTreeMap::new();
        for hint in hints {
            if let MatchHint::Candidate(candidate) = hint {
                let key = (
                    candidate.backend.clone(),
                    candidate.configuration.clone(),
                    candidate.endpoints.clone(),
                );
                match canonical.entry(key) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert(candidate);
                    }
                    std::collections::btree_map::Entry::Occupied(mut entry) => {
                        entry.get_mut().add_evidence(candidate.evidence);
                    }
                }
            }
        }
        let mut candidates: Vec<_> = canonical.into_values().collect();
        match candidates.len() {
            0 => Self::NoMatch,
            1 => Self::Candidate(candidates.remove(0)),
            _ => Self::Ambiguous(candidates),
        }
    }
}

pub(crate) trait Backend: Send + Sync {
    fn descriptor(&self) -> &BackendDescriptor;
    /// Metadata-only: active recognition occurs after leases have been transferred.
    fn match_metadata(&self, discovery: &DiscoveryView<'_>) -> MatchHint;
    fn connect(
        &self,
        candidate: BackendCandidate,
        context: ConnectContext,
        leases: LeaseSet,
        options: SessionOptions,
    ) -> BackendFuture<'_, Session>;
}

pub(crate) struct SessionOptions {
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) connect_deadline: Instant,
    pub(crate) event_capacity: usize,
    pub(crate) work_per_poll: usize,
    pub(crate) max_message_bytes: usize,
    pub(crate) max_endpoints: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RequestId(u64);

pub(crate) trait BatteryProtocol: Send {
    fn queue_battery(&mut self, request: RequestId, component: ComponentId) -> Result<()>;
}
pub(crate) trait FirmwareProtocol: Send {
    fn queue_firmware(&mut self, request: RequestId, component: ComponentId) -> Result<()>;
}

/// One driver owns correlation/framing for both pending requests and unsolicited
/// input. It must bound decoder accumulation and never block in a poll method.
/// `max_frame` bounds message payload and variable decoder accumulation; any
/// fixed framing header is bounded separately. Never accumulate indefinitely.
pub(crate) trait ProtocolDriver: Send {
    fn requires_exclusive(&self) -> bool {
        false
    }
    fn poll_connect(
        &mut self,
        cx: &mut Context<'_>,
        channels: &mut LeaseSet,
        max_frame: usize,
    ) -> Poll<Result<()>>;
    fn capability_report(&self, scope: CapabilityScope) -> Result<CapabilityReport>;
    fn queue_capabilities(&mut self, request: RequestId, scope: CapabilityScope) -> Result<()>;
    fn battery(&mut self) -> Option<&mut dyn BatteryProtocol> {
        None
    }
    fn firmware(&mut self) -> Option<&mut dyn FirmwareProtocol> {
        None
    }
    fn poll_message(
        &mut self,
        cx: &mut Context<'_>,
        channels: &mut LeaseSet,
        max_frame: usize,
    ) -> Poll<Result<ProtocolMessage>>;
}

pub(crate) trait BatteryRead: Send {
    fn read_battery(
        &mut self,
        component: ComponentId,
        deadline: Instant,
    ) -> BackendFuture<'_, ObservationDelivery>;
}
pub(crate) trait FirmwareRead: Send {
    fn read_firmware(
        &mut self,
        component: ComponentId,
        deadline: Instant,
    ) -> BackendFuture<'_, ObservationDelivery>;
}

pub(crate) enum ReplyValue {
    Observation(DeviceObservation),
    Capabilities(CapabilityReport),
}
pub(crate) enum ProtocolMessage {
    Reply {
        request: RequestId,
        source: EndpointId,
        value: ReplyValue,
    },
    Event {
        source: EndpointId,
        component: ComponentId,
        value: DeviceObservation,
    },
}

/// Created by shared orchestration, never trusted without live binding validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObservationDelivery {
    session: BackendSessionId,
    source: EndpointId,
    component: ComponentId,
    value: DeviceObservation,
    ingress: crate::model::time::IngressStamp,
}
impl ObservationDelivery {
    pub(crate) fn component(&self) -> ComponentId {
        self.component
    }
    pub(crate) fn source(&self) -> EndpointId {
        self.source
    }
    pub(crate) fn value(&self) -> &DeviceObservation {
        &self.value
    }
    pub(crate) fn ingress(&self) -> crate::model::time::IngressStamp {
        self.ingress
    }
}

pub(crate) struct CapabilityDelivery {
    session: BackendSessionId,
    source: EndpointId,
    scope: CapabilityScope,
    report: CapabilityReport,
}

pub(crate) struct PreparedRefresh {
    session: BackendSessionId,
    token: crate::model::capability::CapabilityRefreshToken,
}
impl PreparedRefresh {
    pub(crate) fn abandon(self, topology: &mut Topology) -> Result<()> {
        topology
            .abandon_capability_refresh(self.token)
            .map_err(|e| {
                BackendError::new(
                    FailureKind::StaleResult,
                    "capability refresh was superseded or invalidated",
                )
                .caused_by(e)
            })
    }
}

#[cfg(test)]
mod conformance_tests;
#[cfg(test)]
mod fake;
#[cfg(test)]
mod state_tests;
#[cfg(test)]
mod tests;
