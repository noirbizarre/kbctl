//! Synthetic mappings over one shared toy protocol; not a hardware backend.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    io,
    sync::{Arc, Mutex},
    task::{Context, Poll, Wake, Waker},
    time::{Duration, Instant},
};

use crate::{
    model::{
        capability::{
            CapabilityEntry, CapabilityOperation as Op, CapabilityReport, CapabilityScope,
            ReportedVersion, Support, VersionKind,
        },
        connection::{ConnectionState, EndpointLocator, TransportKind},
        identity::{ComponentId, EndpointId, KeyboardId},
        identity::{HardwareIdentity, IdentityAbsence},
        observation::{
            BatteryState, ChargingState, DeviceObservation, FirmwareInfo, Observation, Percentage,
        },
        topology::{
            ComponentKind, ComponentSpec, EndpointSpec, PhysicalDeviceSpec, Topology, UsbIds,
        },
    },
    transport::{
        AccessGrant, IoRequest, LeaseSet, OwnershipReservation, TransportError, TransportFailure,
        TransportIo, TransportLease,
    },
};

use super::{
    error::{BackendError, FailureKind, Result},
    *,
};

pub(super) struct ManualClock(Mutex<Instant>);
impl Clock for ManualClock {
    fn now(&self) -> Instant {
        *self.0.lock().unwrap()
    }
}
impl ManualClock {
    pub(super) fn advance(&self, duration: Duration) {
        *self.0.lock().unwrap() += duration;
    }
}

#[derive(Default)]
pub(super) struct WakeCounter(pub(super) std::sync::atomic::AtomicUsize);
impl Wake for WakeCounter {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}
pub(super) fn poll_once<F: Future + ?Sized>(mut future: Pin<&mut F>) -> Poll<F::Output> {
    let waker = Waker::from(Arc::new(WakeCounter::default()));
    future.as_mut().poll(&mut Context::from_waker(&waker))
}
pub(super) fn ready<F: Future>(future: F) -> F::Output {
    let mut future = Box::pin(future);
    // Fakes with immediate queued data may yield for fairness/handshake stages.
    // A permanently pending future is a test error, never an excuse to sleep.
    for _ in 0..64 {
        if let Poll::Ready(value) = poll_once(future.as_mut()) {
            return value;
        }
    }
    panic!("script did not complete after bounded manual polling");
}

struct WireState {
    frames: VecDeque<std::result::Result<Vec<u8>, TransportFailure>>,
    writes: Vec<Vec<u8>>,
    waker: Option<Waker>,
    auto_reply: bool,
}
#[derive(Clone)]
pub(super) struct Wire(Arc<Mutex<WireState>>);
impl Wire {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(WireState {
            frames: VecDeque::from([Ok(vec![0x7e])]),
            writes: vec![],
            waker: None,
            auto_reply: true,
        })))
    }
    pub(super) fn push(&self, bytes: Vec<u8>) {
        self.enqueue(Ok(bytes));
    }
    pub(super) fn fault(&self, failure: TransportFailure) {
        self.enqueue(Err(failure));
    }
    fn enqueue(&self, frame: std::result::Result<Vec<u8>, TransportFailure>) {
        let wake = {
            let mut wire = self.0.lock().unwrap();
            wire.frames.push_back(frame);
            wire.waker.take()
        };
        if let Some(waker) = wake {
            waker.wake();
        }
    }
    pub(super) fn pause_replies(&self) {
        self.0.lock().unwrap().auto_reply = false;
    }
    pub(super) fn clear_frames(&self) {
        self.0.lock().unwrap().frames.clear();
    }
    pub(super) fn writes(&self) -> usize {
        self.0.lock().unwrap().writes.len()
    }
}

struct FakeIo {
    endpoint: EndpointId,
    wire: Wire,
    log: Arc<Mutex<Vec<String>>>,
    closed: bool,
}
impl TransportIo for FakeIo {
    fn kind(&self) -> TransportKind {
        TransportKind::Serial
    }
    fn poll_io(
        &mut self,
        cx: &mut Context<'_>,
        request: IoRequest<'_>,
    ) -> Poll<crate::transport::Result<usize>> {
        let mut wire = self.wire.0.lock().unwrap();
        match request {
            IoRequest::SerialRead(buffer) => match wire.frames.pop_front() {
                Some(Ok(frame)) if frame.len() > buffer.len() => {
                    Poll::Ready(Err(TransportError::ResourceLimit {
                        endpoint: self.endpoint,
                        limit: buffer.len(),
                    }))
                }
                Some(Ok(frame)) => {
                    buffer[..frame.len()].copy_from_slice(&frame);
                    Poll::Ready(Ok(frame.len()))
                }
                Some(Err(kind)) => Poll::Ready(Err(TransportError::Io {
                    endpoint: self.endpoint,
                    kind,
                    source: io::Error::other("synthetic device vanished or denied access"),
                })),
                None => {
                    wire.waker = Some(cx.waker().clone());
                    Poll::Pending
                }
            },
            IoRequest::SerialWrite(bytes) => {
                wire.writes.push(bytes.to_vec());
                if wire.auto_reply {
                    let frame = match bytes {
                        [0x10] => vec![0x90, 73],
                        [0x11] => vec![0x91, b'r', b'4'],
                        [0x12] => vec![0x92],
                        _ => vec![0xff],
                    };
                    wire.frames.push_back(Ok(frame));
                }
                Poll::Ready(Ok(bytes.len()))
            }
            _ => Poll::Ready(Err(TransportError::WrongKind(self.endpoint))),
        }
    }
    fn close(&mut self) {
        if !self.closed {
            self.closed = true;
            self.log
                .lock()
                .unwrap()
                .push(format!("close:{}", self.endpoint));
        }
    }
}
impl Drop for FakeIo {
    fn drop(&mut self) {
        self.log
            .lock()
            .unwrap()
            .push(format!("drop:{}", self.endpoint));
    }
}

struct Reservation {
    endpoints: Vec<EndpointId>,
    reserved: Arc<Mutex<BTreeSet<EndpointId>>>,
    log: Arc<Mutex<Vec<String>>>,
}
impl OwnershipReservation for Reservation {}
impl Drop for Reservation {
    fn drop(&mut self) {
        let mut reserved = self.reserved.lock().unwrap();
        for endpoint in &self.endpoints {
            reserved.remove(endpoint);
        }
        self.log.lock().unwrap().push("reservation".into());
    }
}

pub(super) struct Rig {
    pub(super) topology: Topology,
    pub(super) primary: EndpointId,
    pub(super) component: Option<ComponentId>,
    pub(super) endpoints: Vec<EndpointId>,
    pub(super) keyboard: KeyboardId,
    pub(super) clock: Arc<ManualClock>,
    pub(super) wires: BTreeMap<EndpointId, Wire>,
    pub(super) log: Arc<Mutex<Vec<String>>>,
    reserved: Arc<Mutex<BTreeSet<EndpointId>>>,
}
impl Rig {
    pub(super) fn new(endpoint_count: usize, components: bool) -> Self {
        let mut topology = Topology::new();
        let physical = topology
            .add_physical_device(
                None,
                PhysicalDeviceSpec {
                    name: None,
                    identity: HardwareIdentity::missing(IdentityAbsence::NotReported),
                    usb_ids: Some(UsbIds {
                        vendor: 0xfffe,
                        product: 1,
                    }),
                    interfaces: vec![],
                    endpoints: (0..endpoint_count)
                        .map(|index| EndpointSpec {
                            interface: None,
                            transport: TransportKind::Serial,
                            locator: EndpointLocator::new(format!("fake-{index}")).unwrap(),
                        })
                        .collect(),
                    components: if components {
                        vec![ComponentSpec {
                            name: None,
                            kind: ComponentKind::Body,
                            parent: None,
                        }]
                    } else {
                        vec![]
                    },
                },
            )
            .unwrap();
        let endpoints: Vec<_> = topology
            .endpoints(physical)
            .map(|endpoint| endpoint.id())
            .collect();
        for endpoint in &endpoints {
            topology.revalidate_endpoint(*endpoint).unwrap();
        }
        let component = topology
            .components(physical)
            .next()
            .map(|component| component.id());
        let keyboard = topology.physical_device(physical).unwrap().keyboard();
        let wires = endpoints.iter().map(|id| (*id, Wire::new())).collect();
        Self {
            topology,
            primary: endpoints[0],
            component,
            endpoints,
            keyboard,
            clock: Arc::new(ManualClock(Mutex::new(Instant::now()))),
            wires,
            log: Arc::default(),
            reserved: Arc::default(),
        }
    }
    pub(super) fn context(&self) -> ConnectContext {
        ConnectContext::new(
            &self.topology,
            self.primary,
            self.endpoints.clone(),
            self.component
                .map(|component| (component, self.primary))
                .into_iter()
                .collect(),
        )
        .unwrap()
    }
    pub(super) fn options(&self) -> SessionOptions {
        SessionOptions {
            clock: self.clock.clone(),
            connect_deadline: self.deadline(),
            event_capacity: 4,
            work_per_poll: 4,
            max_message_bytes: 1024,
            max_endpoints: 8,
        }
    }
    pub(super) fn deadline(&self) -> Instant {
        self.clock.now() + Duration::from_secs(10)
    }
    pub(super) fn leases(&self, grant: AccessGrant) -> crate::transport::Result<LeaseSet> {
        let mut reserved = self.reserved.lock().unwrap();
        if self
            .endpoints
            .iter()
            .any(|endpoint| reserved.contains(endpoint))
        {
            return Err(TransportError::InvalidLease(
                "fake opener's cooperative reservation is already held",
            ));
        }
        reserved.extend(self.endpoints.iter().copied());
        drop(reserved);
        let reservation = Reservation {
            endpoints: self.endpoints.clone(),
            reserved: self.reserved.clone(),
            log: self.log.clone(),
        };
        let leases = self
            .endpoints
            .iter()
            .map(|endpoint| {
                let ConnectionState::Ready(generation) =
                    self.topology.endpoint(*endpoint).unwrap().state()
                else {
                    panic!("revalidate before opening a new lease");
                };
                TransportLease::new(
                    *endpoint,
                    generation,
                    grant,
                    Box::new(FakeIo {
                        endpoint: *endpoint,
                        wire: self.wires[endpoint].clone(),
                        log: self.log.clone(),
                        closed: false,
                    }),
                    1024,
                )
                .unwrap()
            })
            .collect();
        LeaseSet::new(leases, Box::new(reservation))
    }
    pub(super) fn candidate(&self, backend: &FakeBackend) -> BackendCandidate {
        BackendCandidate::new(
            backend.descriptor.id(),
            backend.profile.label(),
            self.endpoints.clone(),
            vec![MatchEvidence::Transport(TransportKind::Serial)],
        )
        .unwrap()
    }
    pub(super) fn connect(&self, backend: &dyn Backend, candidate: BackendCandidate) -> Session {
        ready(backend.connect(
            candidate,
            self.context(),
            self.leases(AccessGrant::TransportExclusive).unwrap(),
            self.options(),
        ))
        .unwrap()
    }
    pub(super) fn reattach(&mut self) {
        for endpoint in &self.endpoints {
            self.topology.revalidate_endpoint(*endpoint).unwrap();
            self.wires.insert(*endpoint, Wire::new());
        }
    }
    pub(super) fn assert_released(&self) {
        assert!(self.reserved.lock().unwrap().is_empty());
        let log = self.log.lock().unwrap();
        let reservation = log
            .iter()
            .rposition(|line| line == "reservation")
            .expect("reservation dropped");
        for endpoint in &self.endpoints {
            let closed = log
                .iter()
                .rposition(|line| line == &format!("close:{endpoint}"))
                .expect("channel closed");
            let dropped = log
                .iter()
                .rposition(|line| line == &format!("drop:{endpoint}"))
                .expect("I/O owner dropped");
            assert!(closed < dropped && dropped < reservation);
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum Profile {
    Battery,
    Firmware,
    Both,
    Unknown,
    Unsupported,
}
impl Profile {
    fn label(self) -> &'static str {
        match self {
            Self::Battery => "battery",
            Self::Firmware => "firmware",
            Self::Both => "both",
            Self::Unknown => "unknown",
            Self::Unsupported => "unsupported",
        }
    }
}
pub(super) struct FakeBackend {
    descriptor: BackendDescriptor,
    profile: Profile,
    pub(super) exclusive: bool,
}
impl FakeBackend {
    pub(super) fn new(profile: Profile, updates: UpdateStrategy) -> Self {
        Self {
            descriptor: BackendDescriptor::new("synthetic.protocol", updates).unwrap(),
            profile,
            exclusive: false,
        }
    }
}
impl Backend for FakeBackend {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }
    fn match_metadata(&self, discovery: &DiscoveryView<'_>) -> MatchHint {
        if discovery.physical.usb_ids()
            != Some(UsbIds {
                vendor: 0xfffe,
                product: 1,
            })
        {
            return MatchHint::NoMatch;
        }
        let endpoints: Vec<_> = discovery
            .endpoints
            .iter()
            .filter(|endpoint| endpoint.transport() == TransportKind::Serial)
            .map(|endpoint| endpoint.id())
            .collect();
        if endpoints.is_empty() {
            return MatchHint::NoMatch;
        }
        MatchHint::Candidate(
            BackendCandidate::new(
                self.descriptor.id(),
                self.profile.label(),
                endpoints,
                vec![MatchEvidence::UsbModel(UsbIds {
                    vendor: 0xfffe,
                    product: 1,
                })],
            )
            .unwrap(),
        )
    }
    fn connect(
        &self,
        candidate: BackendCandidate,
        context: ConnectContext,
        leases: LeaseSet,
        options: SessionOptions,
    ) -> BackendFuture<'_, Session> {
        let configuration_matches = candidate.configuration() == self.profile.label();
        let core = WireProtocol {
            sources: context.endpoints().collect(),
            event_component: context.component_routes().next().map(|(id, _)| id),
            primary: context.primary(),
            stage: 0,
            pending: None,
            profile: self.profile,
            updates: self.descriptor.updates(),
            exclusive: self.exclusive,
            configuration_matches,
        };
        let driver: Box<dyn ProtocolDriver> = match self.profile {
            Profile::Battery => Box::new(BatteryDriver(core)),
            Profile::Firmware => Box::new(FirmwareDriver(core)),
            Profile::Both => Box::new(BothDriver(core)),
            Profile::Unknown | Profile::Unsupported => Box::new(PlainDriver(core)),
        };
        Session::connect(
            &self.descriptor,
            candidate,
            context,
            leases,
            driver,
            options,
        )
    }
}

#[derive(Clone, Copy)]
enum Read {
    Battery(ComponentId),
    Firmware(ComponentId),
    Capabilities(CapabilityScope),
}
struct Pending {
    id: RequestId,
    read: Read,
    sent: bool,
}
struct WireProtocol {
    sources: Vec<EndpointId>,
    primary: EndpointId,
    event_component: Option<ComponentId>,
    stage: usize,
    pending: Option<Pending>,
    profile: Profile,
    updates: UpdateStrategy,
    exclusive: bool,
    configuration_matches: bool,
}

fn battery(level: u16) -> Result<DeviceObservation> {
    Ok(DeviceObservation::Battery(BatteryState {
        level: Observation::Known(Percentage::new(level).map_err(|error| {
            BackendError::new(FailureKind::Protocol, "invalid battery reply").caused_by(error)
        })?),
        charging: Observation::Known(ChargingState::Discharging),
    }))
}

impl WireProtocol {
    fn capabilities(&self, _scope: CapabilityScope) -> Result<CapabilityReport> {
        let mut entries = match self.profile {
            Profile::Battery => vec![(Op::ReadBattery, Support::Supported)],
            Profile::Firmware => vec![(Op::ReadFirmware, Support::Supported)],
            Profile::Both => vec![
                (Op::ReadBattery, Support::Supported),
                (Op::ReadFirmware, Support::Supported),
            ],
            Profile::Unknown => vec![],
            Profile::Unsupported => vec![(Op::ReadBattery, Support::Unsupported)],
        };
        if matches!(
            self.updates,
            UpdateStrategy::Push | UpdateStrategy::Hybrid { .. }
        ) {
            entries.push((Op::SubscribeRuntimeEvents, Support::Supported));
        }
        Ok(CapabilityReport::new(
            "synthetic.protocol",
            vec![],
            entries
                .into_iter()
                .map(|(op, support)| (op, CapabilityEntry::new(support, vec![])))
                .collect(),
        )
        .unwrap())
    }
    fn queue(&mut self, id: RequestId, read: Read) -> Result<()> {
        if self.pending.is_some() {
            return Err(BackendError::new(
                FailureKind::Busy,
                "wire request already pending",
            ));
        }
        self.pending = Some(Pending {
            id,
            read,
            sent: false,
        });
        Ok(())
    }
    fn connect(
        &mut self,
        cx: &mut Context<'_>,
        channels: &mut LeaseSet,
        max_frame: usize,
    ) -> Poll<Result<()>> {
        if !self.configuration_matches {
            return Poll::Ready(Err(BackendError::new(
                FailureKind::InvalidInput,
                "mapping selected an unknown protocol configuration",
            )));
        }
        let source = self.sources[self.stage];
        let mut buffer = vec![0; max_frame];
        let size = match channels
            .channel(source)?
            .poll_io(cx, IoRequest::SerialRead(&mut buffer))
        {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(result) => result?,
        };
        if buffer[..size] != [0x7e] {
            return Poll::Ready(Err(BackendError::new(
                FailureKind::Protocol,
                "invalid synthetic handshake",
            )));
        }
        self.stage += 1;
        if self.stage == self.sources.len() {
            Poll::Ready(Ok(()))
        } else {
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
    fn message(
        &mut self,
        cx: &mut Context<'_>,
        channels: &mut LeaseSet,
        max_frame: usize,
    ) -> Poll<Result<ProtocolMessage>> {
        if let Some(pending) = &mut self.pending
            && !pending.sent
        {
            let command = match pending.read {
                Read::Battery(_) => 0x10,
                Read::Firmware(_) => 0x11,
                Read::Capabilities(_) => 0x12,
            };
            match channels
                .channel(self.primary)?
                .poll_io(cx, IoRequest::SerialWrite(&[command]))
            {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(result) => {
                    result?;
                    pending.sent = true;
                }
            }
        }
        // The fake has one bounded frame per transport read, so there is no
        // unbounded decoder accumulation or request-only input path.
        for source in &self.sources {
            let mut buffer = vec![0; max_frame];
            let size = match channels
                .channel(*source)?
                .poll_io(cx, IoRequest::SerialRead(&mut buffer))
            {
                Poll::Pending => continue,
                Poll::Ready(result) => result?,
            };
            let frame = &buffer[..size];
            if let [0xa0, level] = frame {
                let Some(component) = self.event_component else {
                    return Poll::Ready(Err(BackendError::new(
                        FailureKind::InvalidTarget,
                        "event has no configured component",
                    )));
                };
                let percentage = Percentage::new(u16::from(*level)).map_err(|error| {
                    BackendError::new(FailureKind::Protocol, "invalid battery event")
                        .caused_by(error)
                })?;
                return Poll::Ready(Ok(ProtocolMessage::Event {
                    source: *source,
                    component,
                    value: DeviceObservation::Battery(BatteryState {
                        level: Observation::Known(percentage),
                        charging: Observation::Unknown,
                    }),
                }));
            }
            let Some(pending) = self.pending.take() else {
                return Poll::Ready(Err(BackendError::new(
                    FailureKind::Protocol,
                    "unmatched wire reply",
                )));
            };
            let value = match frame {
                [0x90, level] => ReplyValue::Observation(battery(u16::from(*level))?),
                [0x91, version @ ..] => {
                    let version = String::from_utf8(version.to_vec()).map_err(|error| {
                        BackendError::new(FailureKind::Protocol, "firmware version is not UTF-8")
                            .caused_by(error)
                    })?;
                    ReplyValue::Observation(DeviceObservation::Firmware(Observation::Known(
                        FirmwareInfo::new(vec![
                            ReportedVersion::new(
                                VersionKind::Firmware,
                                "synthetic/firmware",
                                version,
                            )
                            .map_err(|error| {
                                BackendError::new(FailureKind::Protocol, "invalid version")
                                    .caused_by(error)
                            })?,
                        ])
                        .unwrap(),
                    )))
                }
                [0x92] => {
                    let scope = match pending.read {
                        Read::Capabilities(scope) => scope,
                        Read::Battery(id) | Read::Firmware(id) => CapabilityScope::Component(id),
                    };
                    ReplyValue::Capabilities(self.capabilities(scope)?)
                }
                [0x93] => {
                    return Poll::Ready(Ok(ProtocolMessage::Reply {
                        request: RequestId(u64::MAX),
                        source: *source,
                        value: ReplyValue::Observation(battery(1)?),
                    }));
                }
                _ => {
                    return Poll::Ready(Err(BackendError::new(
                        FailureKind::Protocol,
                        "unrecognized synthetic frame",
                    )));
                }
            };
            return Poll::Ready(Ok(ProtocolMessage::Reply {
                request: pending.id,
                source: *source,
                value,
            }));
        }
        Poll::Pending
    }
}

// Separate feature adapters deliberately omit unrelated protocol interfaces;
// every profile shares exactly the same wire pump and lifecycle implementation.
struct BatteryDriver(WireProtocol);
struct FirmwareDriver(WireProtocol);
struct BothDriver(WireProtocol);
struct PlainDriver(WireProtocol);
macro_rules! driver {
    ($name:ident, $($feature:item),* $(,)?) => {
        impl ProtocolDriver for $name {
            fn requires_exclusive(&self) -> bool { self.0.exclusive }
            fn poll_connect(&mut self, cx: &mut Context<'_>, channels: &mut LeaseSet, bound: usize) -> Poll<Result<()>> { self.0.connect(cx, channels, bound) }
            fn capability_report(&self, scope: CapabilityScope) -> Result<CapabilityReport> { self.0.capabilities(scope) }
            fn queue_capabilities(&mut self, id: RequestId, scope: CapabilityScope) -> Result<()> { self.0.queue(id, Read::Capabilities(scope)) }
            fn poll_message(&mut self, cx: &mut Context<'_>, channels: &mut LeaseSet, bound: usize) -> Poll<Result<ProtocolMessage>> { self.0.message(cx, channels, bound) }
            $($feature)*
        }
    }
}
driver!(
    BatteryDriver,
    fn battery(&mut self) -> Option<&mut dyn BatteryProtocol> {
        Some(self)
    }
);
driver!(
    FirmwareDriver,
    fn firmware(&mut self) -> Option<&mut dyn FirmwareProtocol> {
        Some(self)
    }
);
driver!(
    BothDriver,
    fn battery(&mut self) -> Option<&mut dyn BatteryProtocol> {
        Some(self)
    },
    fn firmware(&mut self) -> Option<&mut dyn FirmwareProtocol> {
        Some(self)
    }
);
driver!(PlainDriver,);
impl BatteryProtocol for BatteryDriver {
    fn queue_battery(&mut self, id: RequestId, component: ComponentId) -> Result<()> {
        self.0.queue(id, Read::Battery(component))
    }
}
impl BatteryProtocol for BothDriver {
    fn queue_battery(&mut self, id: RequestId, component: ComponentId) -> Result<()> {
        self.0.queue(id, Read::Battery(component))
    }
}
impl FirmwareProtocol for FirmwareDriver {
    fn queue_firmware(&mut self, id: RequestId, component: ComponentId) -> Result<()> {
        self.0.queue(id, Read::Firmware(component))
    }
}
impl FirmwareProtocol for BothDriver {
    fn queue_firmware(&mut self, id: RequestId, component: ComponentId) -> Result<()> {
        self.0.queue(id, Read::Firmware(component))
    }
}
