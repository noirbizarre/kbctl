//! Synthetic mappings over one shared toy protocol; not a hardware backend.

use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};

pub(super) use crate::test_support::{
    clock::ManualClock,
    poll::{WakeCounter, poll_once, ready},
};
use crate::test_support::{
    device::{DeviceBuilder, DevicePart},
    transport::{Controller, LeaseFactory, Operation, Script},
};
use crate::{
    model::{
        capability::{
            CapabilityEntry, CapabilityOperation as Op, CapabilityReport, CapabilityScope,
            ReportedVersion, Support, VersionKind,
        },
        connection::{EndpointLocator, TransportKind},
        identity::{ComponentId, EndpointId, KeyboardId},
        identity::{HardwareIdentity, IdentityAbsence},
        observation::{
            BatteryState, ChargingState, DeviceObservation, FirmwareInfo, Observation, Percentage,
        },
        topology::{
            ComponentKind, ComponentSpec, EndpointSpec, PhysicalDeviceSpec, Topology, UsbIds,
        },
    },
    transport::{AccessGrant, IoRequest, LeaseSet, TransportFailure},
};

use super::{
    error::{BackendError, FailureKind, Result},
    *,
};

#[derive(Clone)]
pub(super) struct Wire {
    pub(super) control: Controller,
    auto_reply: Arc<AtomicBool>,
}
pub(super) fn frame(payload: &[u8]) -> Vec<u8> {
    let length = u16::try_from(payload.len()).expect("toy frame payload fits u16");
    let mut encoded = length.to_le_bytes().to_vec();
    encoded.extend_from_slice(payload);
    encoded
}
impl Wire {
    pub(super) fn new() -> Self {
        let control = Controller::new(
            "synthetic serial v1",
            TransportKind::Serial,
            Script {
                idle: std::collections::BTreeSet::from([Operation::SerialRead]),
                ..Script::default()
            },
        )
        .unwrap();
        let auto_reply = Arc::new(AtomicBool::new(true));
        let enabled = auto_reply.clone();
        control
            .reactive_writes(
                vec![vec![0x10, 0], vec![0x11, 0], vec![0x12, 0]],
                Arc::new(move |command| {
                    if !enabled.load(Ordering::Relaxed) {
                        return vec![];
                    }
                    let payload = match command {
                        [0x10, 0] => vec![0x90, 73],
                        [0x11, 0] => vec![0x91, b'r', b'4'],
                        [0x12, 0] => vec![0x92],
                        _ => unreachable!("only exact declared commands complete"),
                    };
                    vec![frame(&payload)]
                }),
            )
            .unwrap();
        control.input(frame(&[0x7e]));
        Self {
            control,
            auto_reply,
        }
    }
    pub(super) fn scripted(control: Controller) -> Self {
        Self {
            control,
            auto_reply: Arc::new(AtomicBool::new(false)),
        }
    }
    pub(super) fn push(&self, bytes: Vec<u8>) {
        self.control.input(frame(&bytes));
    }
    pub(super) fn push_raw(&self, bytes: Vec<u8>) {
        self.control.input(bytes);
    }
    pub(super) fn fault(&self, failure: TransportFailure) {
        self.control.fault(Operation::SerialRead, failure, false);
    }
    pub(super) fn pause_replies(&self) {
        self.auto_reply.store(false, Ordering::Relaxed);
    }
    pub(super) fn clear_frames(&self) {
        self.control.clear_input();
    }
    pub(super) fn writes(&self) -> usize {
        self.control.completed_writes().len()
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
    pub(super) factory: LeaseFactory,
}
impl Rig {
    pub(super) fn new(endpoint_count: usize, components: bool) -> Self {
        assert!(
            endpoint_count > 0,
            "connected Rig needs a primary endpoint; use DeviceBuilder for discovery-only fixtures"
        );
        let mut fixture = DeviceBuilder::new()
            .physical(DevicePart {
                alias: "main".into(),
                group: "keyboard".into(),
                interfaces: vec![],
                endpoints: (0..endpoint_count)
                    .map(|index| format!("port-{index}"))
                    .collect(),
                components: if components {
                    vec!["body".into()]
                } else {
                    vec![]
                },
                spec: PhysicalDeviceSpec {
                    name: None,
                    identity: HardwareIdentity::missing(IdentityAbsence::NotReported),
                    usb_ids: Some(UsbIds {
                        vendor: 0xfffe,
                        product: 1,
                    }),
                    interfaces: vec![],
                    serial: None,
                    endpoints: (0..endpoint_count)
                        .map(|index| EndpointSpec {
                            interface: None,
                            transport: TransportKind::Serial,
                            locator: EndpointLocator::new(format!("fake-{index}")).unwrap(),
                            hid: Default::default(),
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
            })
            .build()
            .unwrap();
        fixture.revalidate().unwrap();
        let topology = fixture.topology;
        let physical = fixture.physical["main"];
        let endpoints: Vec<_> = topology
            .endpoints(physical)
            .map(|endpoint| endpoint.id())
            .collect();
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
            clock: Arc::new(ManualClock::new()),
            wires,
            factory: LeaseFactory::default(),
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
        self.leases_from(&self.topology, grant)
    }
    pub(super) fn leases_from(
        &self,
        topology: &Topology,
        grant: AccessGrant,
    ) -> crate::transport::Result<LeaseSet> {
        self.leases_for(topology, &self.endpoints, grant)
    }
    pub(super) fn leases_for(
        &self,
        topology: &Topology,
        endpoints: &[EndpointId],
        grant: AccessGrant,
    ) -> crate::transport::Result<LeaseSet> {
        let channels = self
            .wires
            .iter()
            .map(|(id, wire)| (*id, wire.control.clone()))
            .collect();
        self.factory
            .open(topology, endpoints, &channels, grant, 1024)
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
        }
        self.reopen_wires();
    }
    pub(super) fn reopen_wires(&mut self) {
        for endpoint in &self.endpoints {
            self.wires.insert(*endpoint, Wire::new());
        }
    }
    pub(super) fn assert_released(&self) {
        self.factory.assert_released();
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
    pub(super) support_override: Option<(Op, Support)>,
}
impl FakeBackend {
    pub(super) fn new(profile: Profile, updates: UpdateStrategy) -> Self {
        Self {
            descriptor: BackendDescriptor::new("synthetic.protocol", updates).unwrap(),
            profile,
            exclusive: false,
            support_override: None,
        }
    }
}
impl Backend for FakeBackend {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }
    fn match_metadata(&self, discovery: &DiscoveryView<'_>) -> MatchHint {
        if discovery.physical().usb_ids()
            != Some(UsbIds {
                vendor: 0xfffe,
                product: 1,
            })
        {
            return MatchHint::NoMatch;
        }
        let endpoints: Vec<_> = discovery
            .endpoints()
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
            routes: context.component_routes().collect(),
            primary: context.primary(),
            stage: 0,
            decoders: BTreeMap::new(),
            pending: None,
            profile: self.profile,
            support_override: self.support_override,
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
    sent: usize,
}
struct WireProtocol {
    sources: Vec<EndpointId>,
    primary: EndpointId,
    routes: BTreeMap<ComponentId, EndpointId>,
    stage: usize,
    decoders: BTreeMap<EndpointId, Decoder>,
    pending: Option<Pending>,
    profile: Profile,
    support_override: Option<(Op, Support)>,
    updates: UpdateStrategy,
    exclusive: bool,
    configuration_matches: bool,
}

#[derive(Default)]
struct Decoder {
    header: [u8; 2],
    header_read: usize,
    payload: Vec<u8>,
    payload_read: usize,
}
impl Decoder {
    fn poll(
        &mut self,
        cx: &mut Context<'_>,
        channel: &mut crate::transport::TransportLease,
        bound: usize,
    ) -> Poll<Result<Vec<u8>>> {
        for _ in 0..8 {
            let header = self.header_read < 2;
            let buffer = if header {
                &mut self.header[self.header_read..]
            } else {
                &mut self.payload[self.payload_read..]
            };
            let size = match channel.poll_io(cx, IoRequest::SerialRead(buffer)) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(result) => result?,
            };
            if size == 0 {
                let incomplete = self.header_read > 0;
                return Poll::Ready(Err(BackendError::new(
                    if incomplete {
                        FailureKind::Protocol
                    } else {
                        FailureKind::Disconnected
                    },
                    if incomplete {
                        "EOF truncated a synthetic frame"
                    } else {
                        "EOF before a synthetic frame"
                    },
                )
                .context(
                    "synthetic.protocol",
                    error::Phase::Read,
                    None,
                    Some(channel.endpoint()),
                )
                .caused_by(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "synthetic stream ended",
                ))));
            }
            if header {
                self.header_read += size;
                if self.header_read == 2 {
                    let length = usize::from(u16::from_le_bytes(self.header));
                    if length == 0 || length > bound {
                        return Poll::Ready(Err(BackendError::new(
                            if length == 0 {
                                FailureKind::Protocol
                            } else {
                                FailureKind::ResourceLimit
                            },
                            "invalid synthetic frame length",
                        )
                        .context(
                            "synthetic.protocol",
                            error::Phase::Read,
                            None,
                            Some(channel.endpoint()),
                        )));
                    }
                    self.payload.resize(length, 0);
                }
            } else {
                self.payload_read += size;
                if self.payload_read == self.payload.len() {
                    let payload = std::mem::take(&mut self.payload);
                    self.header_read = 0;
                    self.payload_read = 0;
                    return Poll::Ready(Ok(payload));
                }
            }
        }
        // A ready one-byte source can require several polls for a long frame.
        // Yield with a wake rather than monopolizing one executor turn.
        cx.waker().wake_by_ref();
        Poll::Pending
    }
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
    fn component_for(&self, endpoint: EndpointId) -> Option<ComponentId> {
        self.routes
            .iter()
            .find(|(_, source)| **source == endpoint)
            .map(|(component, _)| *component)
    }
    fn source_for(&self, read: Read) -> Result<EndpointId> {
        match read {
            Read::Capabilities(CapabilityScope::Keyboard(_)) => Ok(self.primary),
            Read::Battery(component)
            | Read::Firmware(component)
            | Read::Capabilities(CapabilityScope::Component(component)) => {
                self.routes.get(&component).copied().ok_or_else(|| {
                    BackendError::new(
                        FailureKind::InvalidTarget,
                        "toy protocol has no route for target",
                    )
                })
            }
        }
    }
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
        if let Some((operation, support)) = self.support_override {
            if let Some((_, current)) = entries.iter_mut().find(|(op, _)| *op == operation) {
                *current = support;
            } else {
                entries.push((operation, support));
            }
        }
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
        self.pending = Some(Pending { id, read, sent: 0 });
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
        let payload = match self.decoders.entry(source).or_default().poll(
            cx,
            channels.channel(source)?,
            max_frame,
        ) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(result) => result?,
        };
        if payload != [0x7e] {
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
        let request_source = self
            .pending
            .as_ref()
            .map(|pending| self.source_for(pending.read))
            .transpose()?;
        if let Some(pending) = &mut self.pending
            && pending.sent < 2
        {
            let command = match pending.read {
                Read::Battery(_) => 0x10,
                Read::Firmware(_) => 0x11,
                Read::Capabilities(_) => 0x12,
            };
            match channels
                .channel(request_source.expect("pending request source"))?
                .poll_io(cx, IoRequest::SerialWrite(&[command, 0][pending.sent..]))
            {
                // Serial lanes are independent: unsolicited input can progress
                // even when output is gated or only partially accepted.
                Poll::Pending => {}
                Poll::Ready(result) => {
                    let size = result?;
                    if size == 0 {
                        return Poll::Ready(Err(BackendError::new(
                            FailureKind::Protocol,
                            "zero progress writing a nonempty synthetic command",
                        )
                        .caused_by(std::io::Error::new(
                            std::io::ErrorKind::WriteZero,
                            "zero-byte command write",
                        ))));
                    }
                    pending.sent += size;
                    if pending.sent < 2 {
                        cx.waker().wake_by_ref();
                    }
                }
            }
        }
        // Each endpoint retains incomplete framing state. Transport chunking is
        // never interpreted as a protocol message boundary.
        for source in &self.sources {
            let buffer = match self.decoders.entry(*source).or_default().poll(
                cx,
                channels.channel(*source)?,
                max_frame,
            ) {
                Poll::Pending => continue,
                Poll::Ready(result) => result?,
            };
            let frame = buffer.as_slice();
            if let [0xa0, level] = frame {
                let Some(component) = self.component_for(*source) else {
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
            let status = match frame {
                [0xa1, level] => Some(DeviceObservation::Patch(
                    crate::model::observation::DevicePatch::battery(
                        Some(Observation::Known(
                            Percentage::new(u16::from(*level)).map_err(|error| {
                                BackendError::new(FailureKind::Protocol, "invalid partial battery")
                                    .caused_by(error)
                            })?,
                        )),
                        None,
                    ),
                )),
                [0xa7, level] => Some(DeviceObservation::Battery(BatteryState {
                    level: Observation::Known(Percentage::new(u16::from(*level)).map_err(
                        |error| {
                            BackendError::new(FailureKind::Protocol, "invalid battery event")
                                .caused_by(error)
                        },
                    )?),
                    charging: Observation::Known(ChargingState::Charging),
                })),
                [0xa4] => Some(DeviceObservation::Firmware(Observation::Known(
                    FirmwareInfo::new(vec![
                        ReportedVersion::new(VersionKind::Firmware, "synthetic/firmware", "event")
                            .unwrap(),
                    ])
                    .unwrap(),
                ))),
                [0xae] => Some(DeviceObservation::Patch(
                    crate::model::observation::DevicePatch::battery(
                        Some(Observation::Unavailable(
                            crate::model::observation::ReportedUnavailable::Sleeping,
                        )),
                        None,
                    ),
                )),
                [0xaf] => Some(DeviceObservation::Patch(status_patch())),
                _ => None,
            };
            if let Some(value) = status {
                let component = self.component_for(*source).ok_or_else(|| {
                    BackendError::new(
                        FailureKind::InvalidTarget,
                        "status frame has no configured component",
                    )
                })?;
                return Poll::Ready(Ok(ProtocolMessage::Event {
                    source: *source,
                    component,
                    value,
                }));
            }
            let Some(pending) = self.pending.take() else {
                return Poll::Ready(Err(BackendError::new(
                    FailureKind::Protocol,
                    "unmatched wire reply",
                )));
            };
            if pending.sent != 2 {
                return Poll::Ready(Err(BackendError::new(
                    FailureKind::Protocol,
                    "reply arrived before command transmission completed",
                )));
            }
            let value = match frame {
                [0x90, level] => ReplyValue::Observation(battery(u16::from(*level))?),
                [0x94, level] => ReplyValue::Observation(DeviceObservation::Patch(
                    crate::model::observation::DevicePatch::battery(
                        Some(Observation::Known(
                            Percentage::new(u16::from(*level)).map_err(|error| {
                                BackendError::new(FailureKind::Protocol, "invalid partial reply")
                                    .caused_by(error)
                            })?,
                        )),
                        None,
                    ),
                )),
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

fn status_patch() -> crate::model::observation::DevicePatch {
    use crate::model::observation::{
        DeviceIndicator, DevicePatch, LightingZone, MeasurementTime, NamespacedId, Reachability,
        Rgb,
    };
    // This fixed toy frame tests normalized status families, not a vendor schema.
    DevicePatch::indicator(DeviceIndicator::CapsLock, Observation::Known(false))
        .merge(DevicePatch::layers(Observation::Known(vec![])).unwrap())
        .unwrap()
        .merge(DevicePatch::lighting_enabled(
            LightingZone::Component,
            Observation::Known(false),
        ))
        .unwrap()
        .merge(DevicePatch::brightness(
            LightingZone::Component,
            Observation::Known(Percentage::new(0).unwrap()),
        ))
        .unwrap()
        .merge(DevicePatch::color(
            LightingZone::Component,
            Observation::Known(Rgb {
                red: 0,
                green: 0,
                blue: 0,
            }),
        ))
        .unwrap()
        .merge(DevicePatch::effect(
            LightingZone::Component,
            Observation::Known(NamespacedId::new("toy/effect", "quiet").unwrap()),
        ))
        .unwrap()
        .merge(DevicePatch::connection(Observation::Known(
            Reachability::Sleeping,
        )))
        .unwrap()
        .measured_at(MeasurementTime {
            clock: NamespacedId::new("toy/clock", "ticks").unwrap(),
            ticks: 17,
        })
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
