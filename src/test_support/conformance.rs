//! Shared assertions, with protocol-specific recipes supplied by backend authors.
use super::{
    clock::ManualClock,
    poll::{poll_once, ready},
    transport::{Controller, LeaseFactory},
};
use crate::{
    backends::{
        Backend, BackendCandidate, BackendFuture, ConnectContext, ObservationDelivery, Session,
        SessionObserver, SessionOptions, error::FailureKind, session::SessionState,
    },
    model::{
        capability::{CapabilityOperation, CapabilityScope, Support},
        identity::{ComponentId, EndpointId},
        observation::DeviceObservation,
        time::Clock,
        topology::Topology,
    },
    transport::AccessGrant,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    task::Poll,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Copy)]
pub(crate) enum Scenario {
    Ready,
    Exchange,
    PendingConnect,
    PendingRead,
    Malformed,
    Disconnect,
    Unknown,
    Unsupported,
    Event,
}
pub(crate) struct Bounds {
    pub(crate) timeout: Duration,
    pub(crate) event_capacity: usize,
    pub(crate) work_per_poll: usize,
    pub(crate) max_message_bytes: usize,
    pub(crate) max_endpoints: usize,
    pub(crate) max_transfer: usize,
}
impl Default for Bounds {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(10),
            event_capacity: 4,
            work_per_poll: 4,
            max_message_bytes: 1024,
            max_endpoints: 8,
            max_transfer: 4096,
        }
    }
}
pub(crate) struct Case {
    pub(crate) bounds: Bounds,
    pub(crate) connect_checkpoint: (EndpointId, String),
    pub(crate) topology: Topology,
    pub(crate) backend: Box<dyn Backend>,
    pub(crate) candidate: BackendCandidate,
    pub(crate) primary: EndpointId,
    pub(crate) endpoints: Vec<EndpointId>,
    pub(crate) routes: Vec<(ComponentId, EndpointId)>,
    pub(crate) component: Option<ComponentId>,
    pub(crate) capability_scope: CapabilityScope,
    pub(crate) clock: Arc<ManualClock>,
    pub(crate) channels: BTreeMap<EndpointId, Controller>,
    pub(crate) leases: LeaseFactory,
}
impl Case {
    pub(crate) fn scope(&self) -> CapabilityScope {
        self.capability_scope
    }
    pub(crate) fn deadline(&self) -> Instant {
        self.clock.now() + self.bounds.timeout
    }
    pub(crate) fn options(&self) -> SessionOptions {
        SessionOptions {
            clock: self.clock.clone(),
            connect_deadline: self.deadline(),
            event_capacity: self.bounds.event_capacity,
            work_per_poll: self.bounds.work_per_poll,
            max_message_bytes: self.bounds.max_message_bytes,
            max_endpoints: self.bounds.max_endpoints,
        }
    }
    pub(crate) fn context(&self) -> ConnectContext {
        ConnectContext::new(
            &self.topology,
            self.primary,
            self.endpoints.clone(),
            self.routes.clone(),
        )
        .unwrap()
    }
    pub(crate) fn open(&self) -> crate::transport::LeaseSet {
        self.leases
            .open(
                &self.topology,
                &self.endpoints,
                &self.channels,
                AccessGrant::TransportExclusive,
                self.bounds.max_transfer,
            )
            .unwrap()
    }
    pub(crate) fn connect(&self) -> BackendFuture<'_, Session> {
        self.backend.connect(
            self.candidate.clone(),
            self.context(),
            self.open(),
            self.options(),
        )
    }
    pub(crate) fn transcripts(&self) -> BTreeMap<EndpointId, Vec<super::transport::Trace>> {
        self.channels
            .iter()
            .map(|(id, control)| (*id, control.transcript()))
            .collect()
    }
    pub(crate) fn assert_complete(&self) {
        for controller in self.channels.values() {
            controller.assert_complete().unwrap();
        }
    }
}

/// Every backend can reuse lifecycle/capability assertions; execution and event
/// groups additionally require corresponding recipes, not dummy capability methods.
pub(crate) trait BackendFactory {
    fn case(&self, scenario: Scenario) -> Case;
    fn reporter(&self) -> &str;
    fn expected_support(&self) -> BTreeMap<CapabilityOperation, Support>;
    fn replacement_channels(&self, case: &Case) -> BTreeMap<EndpointId, Controller>;
}
pub(crate) trait ReadFactory: BackendFactory {
    fn operation(&self) -> CapabilityOperation;
    fn execute_before<'a>(
        &'a self,
        session: &'a mut Session,
        case: &Case,
        deadline: Instant,
    ) -> BackendFuture<'a, ObservationDelivery>;
    fn execute<'a>(
        &'a self,
        session: &'a mut Session,
        case: &Case,
    ) -> BackendFuture<'a, ObservationDelivery> {
        self.execute_before(session, case, case.deadline())
    }
    fn expected_read(&self) -> DeviceObservation;
    fn expected_interfaces(&self) -> BTreeSet<CapabilityOperation>;
}
pub(crate) trait EventFactory: ReadFactory {
    fn inject_event(&self, case: &Case);
    fn finish_read(&self, case: &Case);
    fn expected_event(&self) -> DeviceObservation;
}

pub(crate) fn lifecycle(factory: &impl BackendFactory) {
    for scenario in [Scenario::Ready, Scenario::PendingConnect] {
        let case = factory.case(scenario);
        let mut future = case.connect();
        if matches!(scenario, Scenario::PendingConnect) {
            assert!(poll_once(future.as_mut()).is_pending());
        }
        drop(future);
        case.leases.assert_released();
        case.channels[&case.connect_checkpoint.0]
            .assert_cancelled_at(&case.connect_checkpoint.1)
            .unwrap();
    }
    let case = factory.case(Scenario::Ready);
    let mut session = ready(case.connect()).unwrap();
    let observer = session.observer();
    let mut receiver = session.take_events().unwrap();
    assert_eq!(session.state(), SessionState::Active);
    session.shutdown();
    session.shutdown();
    session.disconnect();
    assert_eq!(observer.life().state, SessionState::Shutdown);
    assert!(ready(receiver.next()).is_none());
    case.leases.assert_released();
    let case = factory.case(Scenario::Ready);
    let session = ready(case.connect()).unwrap();
    drop(session);
    case.leases.assert_released();
}

pub(crate) fn capabilities(factory: &impl BackendFactory) {
    let mut case = factory.case(Scenario::Ready);
    let mut session = ready(case.connect()).unwrap();
    let report = publish_report(&mut case, &mut session);
    assert_eq!(report.reporter(), factory.reporter());
    assert_eq!(
        report
            .operations()
            .map(|(op, entry)| (op, entry.support()))
            .collect::<BTreeMap<_, _>>(),
        factory.expected_support()
    );
    assert_eq!(
        report.support(CapabilityOperation::WriteMacros),
        Support::Unknown
    );
    session.shutdown();
    case.assert_complete();
    case.leases.assert_released();
}

pub(crate) fn reads(factory: &impl ReadFactory) {
    let case = factory.case(Scenario::Exchange);
    let mut session = ready(case.connect()).unwrap();
    let mut interfaces = BTreeSet::new();
    if session.battery().is_some() {
        interfaces.insert(CapabilityOperation::ReadBattery);
    }
    if session.firmware().is_some() {
        interfaces.insert(CapabilityOperation::ReadFirmware);
    }
    assert_eq!(interfaces, factory.expected_interfaces());
    ready(session.query_capabilities(case.scope(), case.deadline())).unwrap();
    let delivery = ready(factory.execute(&mut session, &case)).unwrap();
    assert_eq!(
        session
            .accept_observation(&case.topology, &delivery)
            .unwrap(),
        &factory.expected_read()
    );
    session.shutdown();
    case.assert_complete();
    case.leases.assert_released();
    for (scenario, expected) in [
        (Scenario::Unknown, FailureKind::SupportUnknown),
        (Scenario::Unsupported, FailureKind::Unsupported),
    ] {
        let mut case = factory.case(scenario);
        let mut session = ready(case.connect()).unwrap();
        let report = publish_report(&mut case, &mut session);
        assert_eq!(
            report.support(factory.operation()),
            if matches!(scenario, Scenario::Unknown) {
                Support::Unknown
            } else {
                Support::Unsupported
            }
        );
        let before = case.transcripts();
        let error = ready(factory.execute(&mut session, &case)).unwrap_err();
        assert_eq!(error.kind(), expected);
        assert_eq!(session.state(), SessionState::Active);
        assert_eq!(case.transcripts(), before);
        session.shutdown();
        case.assert_complete();
        case.leases.assert_released();
    }
}

fn publish_report(
    case: &mut Case,
    session: &mut Session,
) -> crate::model::capability::CapabilityReport {
    let scope = case.scope();
    let prepared = session.prepare_refresh(&mut case.topology, scope).unwrap();
    let report = ready(session.query_capabilities(scope, case.deadline())).unwrap();
    session
        .accept_capabilities(&mut case.topology, prepared, report)
        .unwrap();
    case.topology
        .capabilities(scope)
        .unwrap()
        .report()
        .unwrap()
        .clone()
}

pub(crate) fn cancellation_and_timeouts(factory: &impl ReadFactory) {
    let case = factory.case(Scenario::Ready);
    let mut session = ready(case.connect()).unwrap();
    let before = case.transcripts();
    drop(factory.execute(&mut session, &case));
    assert_eq!(case.transcripts(), before);
    assert_eq!(session.state(), SessionState::Active);
    let error = ready(factory.execute_before(&mut session, &case, case.clock.now())).unwrap_err();
    assert_eq!(error.kind(), FailureKind::DeadlineExceeded);
    assert_eq!(case.transcripts(), before);
    assert_eq!(session.state(), SessionState::Active);
    for expire in [false, true] {
        let case = factory.case(Scenario::PendingRead);
        let mut session = ready(case.connect()).unwrap();
        let observer = session.observer();
        let mut request = factory.execute(&mut session, &case);
        assert!(poll_once(request.as_mut()).is_pending());
        if expire {
            case.clock.advance(case.bounds.timeout);
            let Poll::Ready(Err(error)) = poll_once(request.as_mut()) else {
                panic!("explicit repoll must enforce deadline");
            };
            assert_eq!(error.kind(), FailureKind::DeadlineExceeded);
        }
        drop(request);
        assert_eq!(observer.life().state, SessionState::Failed);
        case.leases.assert_released();
    }
}

pub(crate) fn failures(factory: &impl ReadFactory) {
    for (scenario, expected) in [
        (Scenario::Malformed, FailureKind::Protocol),
        (Scenario::Disconnect, FailureKind::Disconnected),
    ] {
        let case = factory.case(scenario);
        let mut session = ready(case.connect()).unwrap();
        let error = ready(factory.execute(&mut session, &case)).unwrap_err();
        assert_eq!(error.kind(), expected);
        assert_eq!(session.state(), SessionState::Failed);
        case.assert_complete();
        case.leases.assert_released();
    }
}

pub(crate) fn events(factory: &impl EventFactory) {
    let case = factory.case(Scenario::Event);
    let mut session = ready(case.connect()).unwrap();
    let observer: SessionObserver = session.observer();
    let mut receiver = session.take_events().unwrap();
    let mut waiting = Box::pin(receiver.next());
    assert!(poll_once(waiting.as_mut()).is_pending());
    let mut request = factory.execute(&mut session, &case);
    assert!(poll_once(request.as_mut()).is_pending());
    factory.inject_event(&case);
    assert!(poll_once(request.as_mut()).is_pending());
    let event = ready(waiting).unwrap().unwrap();
    assert_eq!(
        observer.accept(&case.topology, &event).unwrap(),
        &factory.expected_event()
    );
    factory.finish_read(&case);
    let reply = ready(request).unwrap();
    assert_eq!(
        observer.accept(&case.topology, &reply).unwrap(),
        &factory.expected_read()
    );
    session.shutdown();
    case.assert_complete();
    case.leases.assert_released();
}

pub(crate) fn reconnect(factory: &impl ReadFactory) {
    let mut case = factory.case(Scenario::Ready);
    let mut session = ready(case.connect()).unwrap();
    let old_id = session.id();
    let old_delivery = ready(factory.execute(&mut session, &case)).unwrap();
    session.disconnect();
    case.leases.assert_released();
    let invalid = ready(session.reconnect(
        case.backend.as_ref(),
        case.candidate.clone(),
        case.context(),
        case.open(),
        case.options(),
    ))
    .unwrap_err();
    assert_eq!(invalid.kind(), FailureKind::InvalidInput);
    for endpoint in &case.endpoints {
        case.topology.revalidate_endpoint(*endpoint).unwrap();
    }
    case.channels = factory.replacement_channels(&case);
    let mut replacement = ready(session.reconnect(
        case.backend.as_ref(),
        case.candidate.clone(),
        case.context(),
        case.open(),
        case.options(),
    ))
    .unwrap();
    assert_ne!(replacement.id(), old_id);
    assert!(
        replacement
            .accept_observation(&case.topology, &old_delivery)
            .is_err()
    );
    assert!(ready(factory.execute(&mut replacement, &case)).is_ok());
    replacement.shutdown();
    case.leases.assert_released();
}
