use super::*;
use crate::{
    backends::{
        BackendDescriptor, BackendFuture, ConnectContext, MatchHint, Session, SessionOptions,
        UpdateStrategy,
        error::{BackendError, FailureKind},
    },
    model::{
        capability::{
            CapabilityEntry, CapabilityOperation, CapabilityReport, CapabilityScope, Support,
        },
        connection::EndpointLocator,
        identity::{HardwareIdentity, IdentityAbsence},
        topology::{DisplayName, EndpointSpec, InterfaceSpec, PhysicalDeviceSpec},
    },
    transport::LeaseSet,
};
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

const USB: UsbIds = UsbIds {
    vendor: 0xfffd,
    product: 1,
};

#[test]
fn metadata_identification_and_topology_inspection_share_exact_physical_predicates() {
    let mut a = definition("fixture.a", serial_channel(None));
    a.selector.serial = Some(AdvertisedSerial::new("Exact ").unwrap());
    let b = definition("fixture.b", serial_channel(None));
    let registry = Registry::new(vec![a, b], sources(), 4096).unwrap();
    for (usb, serial) in [
        (Some(USB), Some("Exact ")),
        (Some(USB), Some("exact ")),
        (Some(USB), None),
        (None, Some("Exact ")),
    ] {
        let mut topology = Topology::new();
        let physical = topology
            .add_physical_device(None, spec(usb, serial, vec![]))
            .unwrap();
        let inspected = registry.inspect(&topology, physical, &[]).unwrap();
        let serial = serial.map(|value| AdvertisedSerial::new(value).unwrap());
        let metadata = registry.identify_metadata(PhysicalMetadata {
            usb,
            serial: serial.as_ref(),
        });
        assert_eq!(metadata.identification, inspected.identification);
        assert_eq!(metadata.assessments, inspected.assessments);
    }
}

fn serial_channel(interface_number: Option<u8>) -> EndpointSelector {
    EndpointSelector {
        transport: TransportKind::Serial,
        interface_number,
        hid: HidMetadata::default(),
    }
}
fn hid_channel(
    interface_number: Option<u8>,
    page: Option<u16>,
    usage: Option<u16>,
) -> EndpointSelector {
    EndpointSelector {
        transport: TransportKind::Hid,
        interface_number,
        hid: HidMetadata {
            usage_page: page,
            usage,
        },
    }
}

/// Every interface number is one host-discovered interface, possibly with several
/// addressable HID collections. These are synthetic descriptors, not captures.
fn spec(
    usb: Option<UsbIds>,
    serial: Option<&str>,
    channels: Vec<EndpointSelector>,
) -> PhysicalDeviceSpec {
    let numbers: BTreeSet<_> = channels
        .iter()
        .filter_map(|channel| channel.interface_number)
        .collect();
    let interfaces: Vec<_> = numbers
        .iter()
        .map(|number| InterfaceSpec {
            name: None,
            usb_number: Some(*number),
        })
        .collect();
    let indexes: BTreeMap<_, _> = numbers
        .into_iter()
        .enumerate()
        .map(|(index, number)| (number, index))
        .collect();
    PhysicalDeviceSpec {
        name: None,
        usb_ids: usb,
        serial: serial.map(|serial| AdvertisedSerial::new(serial).unwrap()),
        identity: HardwareIdentity::missing(IdentityAbsence::NotReported),
        interfaces,
        endpoints: channels
            .into_iter()
            .enumerate()
            .map(|(index, channel)| EndpointSpec {
                interface: channel.interface_number.map(|number| indexes[&number]),
                transport: channel.transport,
                hid: channel.hid,
                locator: EndpointLocator::new(format!("synthetic-{index}")).unwrap(),
            })
            .collect(),
        components: vec![],
    }
}
fn sources() -> Vec<SourceRecord> {
    ["fixture-a", "fixture-b"]
        .into_iter()
        .map(|id| SourceRecord {
            id: id.into(),
            class: SourceClass::Community,
            reference: format!("https://example.invalid/{id}"),
            scope: "synthetic test declaration, not hardware evidence".into(),
        })
        .collect()
}
fn definition(id: &str, selector: EndpointSelector) -> DeviceDefinition {
    DeviceDefinition {
        id: id.into(),
        model: "Synthetic keyboard".into(),
        family: "fixture.keyboard".into(),
        variant: "synthetic".into(),
        mode: DeviceMode::Runtime,
        selector: PhysicalSelector {
            usb: USB,
            serial: None,
        },
        components: vec![ComponentTemplate {
            key: "body".into(),
            kind: ComponentKind::Body,
            parent: None,
        }],
        protocols: vec![ProtocolReference {
            id: "mapping".into(),
            backend: "fixture.protocol".into(),
            configuration: "fixture-config".into(),
            status: MappingStatus::Enabled,
            primary_role: "management".into(),
            roles: vec![EndpointRole {
                key: "management".into(),
                selector,
            }],
            sources: vec!["fixture-a".into()],
        }],
        sources: vec!["fixture-a".into()],
        limitations: "all descriptors and protocol identities are synthetic".into(),
    }
}
fn registry(definitions: Vec<DeviceDefinition>) -> Registry {
    Registry::new(definitions, sources(), 4096).unwrap()
}

#[derive(Clone, Copy)]
enum Behavior {
    Match,
    Decline,
    Foreign(EndpointId),
    WrongConfiguration,
    FalseEvidence,
}
struct StubBackend {
    descriptor: BackendDescriptor,
    behavior: Behavior,
    matches: AtomicUsize,
    connects: AtomicUsize,
    seen_interfaces: Mutex<Vec<Vec<u8>>>,
}
impl StubBackend {
    fn new(behavior: Behavior) -> Self {
        Self {
            descriptor: BackendDescriptor::new("fixture.protocol", UpdateStrategy::Push).unwrap(),
            behavior,
            matches: AtomicUsize::new(0),
            connects: AtomicUsize::new(0),
            seen_interfaces: Mutex::new(vec![]),
        }
    }
}
impl Backend for StubBackend {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }
    fn match_metadata(&self, view: &DiscoveryView<'_>) -> MatchHint {
        self.matches.fetch_add(1, Ordering::Relaxed);
        self.seen_interfaces.lock().unwrap().push(
            view.interfaces()
                .filter_map(|interface| interface.usb_number())
                .collect(),
        );
        if matches!(self.behavior, Behavior::Decline) {
            return MatchHint::NoMatch;
        }
        let endpoints = match self.behavior {
            Behavior::Foreign(endpoint) => vec![endpoint],
            _ => view.endpoints().map(|endpoint| endpoint.id()).collect(),
        };
        let evidence = match self.behavior {
            Behavior::FalseEvidence => MatchEvidence::Interface(99),
            _ => MatchEvidence::UsbModel(view.physical().usb_ids().unwrap()),
        };
        MatchHint::Candidate(
            BackendCandidate::new(
                self.descriptor.id(),
                if matches!(self.behavior, Behavior::WrongConfiguration) {
                    "other-config"
                } else {
                    "fixture-config"
                },
                endpoints,
                vec![evidence],
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
        self.connects.fetch_add(1, Ordering::Relaxed);
        Box::pin(async move {
            drop((candidate, context, leases, options));
            Err(BackendError::new(
                FailureKind::Unsupported,
                "registry tests must never open or connect devices",
            ))
        })
    }
}

#[test]
fn reviewed_builtin_declarations_are_valid_without_registering_any_backend() {
    let registry = builtin::registry().unwrap();
    assert_eq!(registry.definitions().len(), 8);
}

#[test]
fn source_backed_fixtures_identify_all_target_variants_without_claiming_capabilities_or_presence() {
    let registry = builtin::registry().unwrap();
    for (vendor, product, expected, mode, class) in [
        (
            0x320f,
            0x5044,
            "glorious.gmmk-pro.rev1-ansi",
            DeviceMode::Runtime,
            SourceClass::Upstream,
        ),
        (
            0x35ef,
            0x0010,
            "dygma.defy.wired",
            DeviceMode::Runtime,
            SourceClass::Vendor,
        ),
        (
            0x35ef,
            0x0012,
            "dygma.defy.wireless",
            DeviceMode::Runtime,
            SourceClass::Vendor,
        ),
        (
            0x35ef,
            0x0011,
            "dygma.defy.wired.bootloader",
            DeviceMode::Bootloader,
            SourceClass::Vendor,
        ),
        (
            0x35ef,
            0x0013,
            "dygma.defy.wireless.bootloader",
            DeviceMode::Bootloader,
            SourceClass::Vendor,
        ),
        (
            0x37d1,
            0x0064,
            "naya.create.left",
            DeviceMode::Runtime,
            SourceClass::Community,
        ),
        (
            0x37d1,
            0x00c8,
            "naya.create.right",
            DeviceMode::Runtime,
            SourceClass::Community,
        ),
        (
            0x37d1,
            0x012c,
            "naya.create.dongle",
            DeviceMode::Runtime,
            SourceClass::Community,
        ),
    ] {
        let mut topology = Topology::new();
        let physical = topology
            .add_physical_device(
                None,
                spec(
                    Some(UsbIds { vendor, product }),
                    None,
                    vec![serial_channel(None)],
                ),
            )
            .unwrap();
        let inspection = registry.inspect(&topology, physical, &[]).unwrap();
        assert_eq!(
            inspection.identification,
            Identification::Identified(expected.into())
        );
        let matched = &inspection.matches[0];
        assert_eq!(matched.definition.mode, mode);
        for source in &matched.definition.sources {
            let source = registry.source(source).unwrap();
            assert_eq!(source.class, class);
            assert!(source.reference.contains("/blob/"));
            assert!(!source.scope.is_empty());
        }
        assert!(inspection.routes.is_empty());
        assert!(inspection.actionable().is_none());
        if mode == DeviceMode::Bootloader {
            assert!(matched.protocols.is_empty());
        }
        assert!(
            matched
                .protocols
                .iter()
                .all(|protocol| protocol.routing == RouteStatus::ResearchOnly)
        );
        let keyboard = topology.physical_device(physical).unwrap().keyboard();
        assert_eq!(topology.components(physical).count(), 0);
        assert_eq!(
            topology
                .capabilities(CapabilityScope::Keyboard(keyboard))
                .unwrap()
                .support(CapabilityOperation::ReadBattery),
            Support::Unknown
        );
    }
}

#[test]
fn recognized_devices_with_no_management_channel_stay_identified() {
    let registry = builtin::registry().unwrap();
    let mut topology = Topology::new();
    for (vendor, product, expected) in [
        (0x320f, 0x5044, "glorious.gmmk-pro.rev1-ansi"),
        (0x35ef, 0x0010, "dygma.defy.wired"),
        (0x37d1, 0x0064, "naya.create.left"),
    ] {
        let physical = topology
            .add_physical_device(None, spec(Some(UsbIds { vendor, product }), None, vec![]))
            .unwrap();
        let result = registry.inspect(&topology, physical, &[]).unwrap();
        assert_eq!(
            result.identification,
            Identification::Identified(expected.into())
        );
        assert!(result.routes.is_empty());
        assert!(
            result.matches[0]
                .protocols
                .iter()
                .all(|reference| reference.assignment == Assignment::NoEndpoints)
        );
    }
}

#[test]
fn unknown_ids_missing_ids_and_misleading_names_never_reach_a_guessed_backend() {
    let backend = StubBackend::new(Behavior::Match);
    let registry = registry(vec![definition("known", serial_channel(None))]);
    let mut topology = Topology::new();
    for usb in [
        None,
        Some(UsbIds {
            vendor: 0xffff,
            product: 1,
        }),
    ] {
        let mut unit = spec(usb, None, vec![serial_channel(None)]);
        unit.name = Some(DisplayName::new("Synthetic keyboard Dygma Defy").unwrap());
        let physical = topology.add_physical_device(None, unit).unwrap();
        let before = format!("{topology:?}");
        let result = registry.inspect(&topology, physical, &[&backend]).unwrap();
        assert_eq!(result.identification, Identification::Unknown);
        assert!(result.matches.is_empty() && result.routes.is_empty());
        assert_eq!(format!("{topology:?}"), before);
    }
    assert_eq!(backend.matches.load(Ordering::Relaxed), 0);
    assert_eq!(backend.connects.load(Ordering::Relaxed), 0);
    assert_eq!(topology.physical_devices().count(), 2);
}

#[test]
fn exact_serial_predicates_require_available_case_sensitive_evidence_without_promoting_identity() {
    let mut definition = definition("serial-specific", serial_channel(None));
    definition.selector.serial = Some(AdvertisedSerial::new(" Unit-A ").unwrap());
    let registry = registry(vec![definition]);
    for (serial, verdict) in [
        (None, Verdict::Missing),
        (Some("Unit-A"), Verdict::Mismatch),
        (Some(" unit-a "), Verdict::Mismatch),
        (Some(" Unit-A "), Verdict::Satisfied),
    ] {
        let mut topology = Topology::new();
        let physical = topology
            .add_physical_device(None, spec(Some(USB), serial, vec![serial_channel(None)]))
            .unwrap();
        let result = registry.inspect(&topology, physical, &[]).unwrap();
        assert_eq!(result.assessments[0].checks[1].verdict(), verdict);
        assert_eq!(
            matches!(result.identification, Identification::Identified(_)),
            verdict == Verdict::Satisfied
        );
        assert_eq!(
            topology
                .physical_device(physical)
                .unwrap()
                .serial()
                .map(AdvertisedSerial::as_str),
            serial
        );
        assert_eq!(
            topology
                .physical_device(physical)
                .unwrap()
                .identity()
                .absence(),
            Some(IdentityAbsence::NotReported)
        );
    }
    assert!(AdvertisedSerial::new(" \n").is_err());
}

#[test]
fn hid_predicates_cannot_borrow_facts_from_sibling_interfaces_or_collections() {
    let mut topology = Topology::new();
    let physical = topology
        .add_physical_device(
            None,
            spec(
                Some(USB),
                None,
                vec![
                    hid_channel(Some(1), Some(0xff00), Some(1)),
                    hid_channel(Some(1), Some(1), Some(2)),
                    hid_channel(Some(2), Some(0xff00), Some(2)),
                ],
            ),
        )
        .unwrap();
    let registry = registry(vec![definition(
        "collection",
        hid_channel(Some(1), Some(0xff00), Some(2)),
    )]);
    let backend = StubBackend::new(Behavior::Match);
    let result = registry.inspect(&topology, physical, &[&backend]).unwrap();
    assert!(matches!(
        result.identification,
        Identification::Identified(_)
    ));
    let protocol = &result.matches[0].protocols[0];
    assert_eq!(protocol.assignment, Assignment::NoMatch);
    assert!(protocol.roles[0].matches.is_empty());
    assert_eq!(backend.matches.load(Ordering::Relaxed), 0);
}

#[test]
fn missing_and_rejected_collection_metadata_have_distinct_diagnostics() {
    for (metadata, expected) in [
        (
            HidMetadata {
                usage_page: Some(0xff00),
                usage: None,
            },
            Verdict::Missing,
        ),
        (
            HidMetadata {
                usage_page: Some(0xff00),
                usage: Some(9),
            },
            Verdict::Mismatch,
        ),
    ] {
        let mut topology = Topology::new();
        let physical = topology
            .add_physical_device(
                None,
                spec(
                    Some(USB),
                    None,
                    vec![EndpointSelector {
                        transport: TransportKind::Hid,
                        interface_number: Some(3),
                        hid: metadata,
                    }],
                ),
            )
            .unwrap();
        let registry = registry(vec![definition(
            "collection",
            hid_channel(Some(3), Some(0xff00), Some(1)),
        )]);
        let result = registry.inspect(&topology, physical, &[]).unwrap();
        let check = result.matches[0].protocols[0].roles[0].endpoints[0]
            .checks
            .iter()
            .find(|check| matches!(check.expected, Fact::Usage(_)))
            .unwrap();
        assert_eq!(check.verdict(), expected);
        assert_eq!(
            result.matches[0].protocols[0].assignment,
            Assignment::NoMatch
        );
    }
}

#[test]
fn zero_is_a_reported_hid_value_not_a_substitute_for_missing_metadata() {
    let registry = registry(vec![definition(
        "zero",
        hid_channel(None, Some(0), Some(0)),
    )]);
    for (metadata, matches) in [
        (HidMetadata::default(), false),
        (
            HidMetadata {
                usage_page: Some(0),
                usage: Some(0),
            },
            true,
        ),
    ] {
        let mut topology = Topology::new();
        let physical = topology
            .add_physical_device(
                None,
                spec(
                    Some(USB),
                    None,
                    vec![EndpointSelector {
                        transport: TransportKind::Hid,
                        interface_number: None,
                        hid: metadata,
                    }],
                ),
            )
            .unwrap();
        let result = registry.inspect(&topology, physical, &[]).unwrap();
        assert_eq!(
            result.matches[0].protocols[0].roles[0].matches.len(),
            usize::from(matches)
        );
    }
}

#[test]
fn enabled_routes_pass_only_the_selected_owned_interfaces_to_a_registered_backend() {
    let mut topology = Topology::new();
    let physical = topology
        .add_physical_device(
            None,
            spec(
                Some(USB),
                None,
                vec![serial_channel(Some(1)), serial_channel(Some(2))],
            ),
        )
        .unwrap();
    let registry = registry(vec![definition(
        "specific-channel",
        serial_channel(Some(2)),
    )]);
    let backend = StubBackend::new(Behavior::Match);
    let result = registry.inspect(&topology, physical, &[&backend]).unwrap();
    let route = result.actionable().unwrap();
    assert_eq!(
        route.candidate.endpoints(),
        &[topology.endpoints(physical).last().unwrap().id()]
    );
    assert_eq!(*backend.seen_interfaces.lock().unwrap(), [vec![2]]);
    assert_eq!(backend.connects.load(Ordering::Relaxed), 0);
    assert_eq!(result.matches[0].plan.interfaces.len(), 2);
    assert_eq!(topology.keyboards().count(), 1);
    assert_eq!(topology.components(physical).count(), 0);
}

#[test]
fn disabled_absent_and_declined_implementations_never_become_executable_routes() {
    let mut topology = Topology::new();
    let physical = topology
        .add_physical_device(None, spec(Some(USB), None, vec![serial_channel(None)]))
        .unwrap();
    let mut research = definition("research", serial_channel(None));
    research.protocols[0].status = MappingStatus::ResearchOnly;
    let backend = StubBackend::new(Behavior::Match);
    let result = registry(vec![research])
        .inspect(&topology, physical, &[&backend])
        .unwrap();
    assert_eq!(
        result.matches[0].protocols[0].routing,
        RouteStatus::ResearchOnly
    );
    assert!(result.routes.is_empty());
    assert_eq!(backend.matches.load(Ordering::Relaxed), 0);
    let registry = registry(vec![definition("enabled", serial_channel(None))]);
    assert_eq!(
        registry.inspect(&topology, physical, &[]).unwrap().matches[0].protocols[0].routing,
        RouteStatus::BackendUnavailable
    );
    let backend = StubBackend::new(Behavior::Decline);
    let result = registry.inspect(&topology, physical, &[&backend]).unwrap();
    assert_eq!(
        result.matches[0].protocols[0].routing,
        RouteStatus::Declined
    );
    assert!(result.actionable().is_none());
}

#[test]
fn foreign_endpoints_inconsistent_configurations_and_false_evidence_are_contract_errors() {
    let mut topology = Topology::new();
    let physical = topology
        .add_physical_device(None, spec(Some(USB), None, vec![serial_channel(None)]))
        .unwrap();
    let other = topology
        .add_physical_device(None, spec(Some(USB), None, vec![serial_channel(None)]))
        .unwrap();
    let foreign = topology.endpoints(other).next().unwrap().id();
    let registry = registry(vec![definition("known", serial_channel(None))]);
    for behavior in [
        Behavior::Foreign(foreign),
        Behavior::WrongConfiguration,
        Behavior::FalseEvidence,
    ] {
        let backend = StubBackend::new(behavior);
        assert!(matches!(
            registry.inspect(&topology, physical, &[&backend]),
            Err(RegistryError::BackendContract { .. })
        ));
    }
    let backend = StubBackend::new(Behavior::Match);
    assert!(matches!(
        registry.inspect(&topology, physical, &[&backend, &backend]),
        Err(RegistryError::DuplicateBackend(_))
    ));
}

#[test]
fn repeated_routes_merge_provenance_but_conflicting_definitions_remain_ambiguous() {
    let mut topology = Topology::new();
    let physical = topology
        .add_physical_device(None, spec(Some(USB), None, vec![serial_channel(Some(1))]))
        .unwrap();
    let mut a = definition("definition-a", serial_channel(None));
    let mut duplicate = a.protocols[0].clone();
    duplicate.id = "second-reference".into();
    duplicate.sources = vec!["fixture-b".into()];
    a.protocols.push(duplicate);
    let mut b = definition("definition-b", serial_channel(Some(1)));
    b.sources = vec!["fixture-b".into()];
    let backend = StubBackend::new(Behavior::Match);
    let result = registry(vec![b.clone(), a.clone()])
        .inspect(&topology, physical, &[&backend])
        .unwrap();
    assert_eq!(
        result.identification,
        Identification::Ambiguous(vec!["definition-a".into(), "definition-b".into()])
    );
    assert_eq!(result.routes.len(), 1);
    let route = &result.routes[0];
    assert_eq!(route.definitions.len(), 2);
    assert_eq!(route.sources.len(), 2);
    assert_eq!(route.references.len(), 3);
    assert!(
        route
            .candidate
            .evidence()
            .contains(&MatchEvidence::Interface(1))
    );
    assert!(result.actionable().is_none());
    let result = registry(vec![a])
        .inspect(&topology, physical, &[&backend])
        .unwrap();
    assert_eq!(result.routes.len(), 1);
    assert_eq!(result.routes[0].references.len(), 2);
    assert!(result.actionable().is_some());
}

#[test]
fn declaration_order_and_input_subset_order_do_not_change_canonical_results() {
    let mut topology = Topology::new();
    let physical = topology
        .add_physical_device(
            None,
            spec(
                Some(USB),
                None,
                vec![serial_channel(Some(0)), serial_channel(Some(1))],
            ),
        )
        .unwrap();
    let a = definition("a", serial_channel(Some(0)));
    let mut b = definition("b", serial_channel(Some(1)));
    b.sources = vec!["fixture-b".into(), "fixture-a".into()];
    let backend = StubBackend::new(Behavior::Match);
    let first = registry(vec![a.clone(), b.clone()])
        .inspect(&topology, physical, &[&backend])
        .unwrap();
    b.sources.reverse();
    let mut reversed_sources = sources();
    reversed_sources.reverse();
    let second = Registry::new(vec![b, a], reversed_sources, 4096)
        .unwrap()
        .inspect(&topology, physical, &[&backend])
        .unwrap();
    assert_eq!(first, second);
    let view = DiscoveryView::new(&topology, physical).unwrap();
    let ids: Vec<_> = view.endpoints().map(|endpoint| endpoint.id()).collect();
    let mut reversed = ids.clone();
    reversed.reverse();
    assert_eq!(
        view.restricted(&ids)
            .unwrap()
            .endpoints()
            .map(|endpoint| endpoint.id())
            .collect::<Vec<_>>(),
        view.restricted(&reversed)
            .unwrap()
            .endpoints()
            .map(|endpoint| endpoint.id())
            .collect::<Vec<_>>()
    );
}

#[test]
fn two_naya_pairs_with_missing_or_duplicate_advertised_serials_stay_separate() {
    let registry = builtin::registry().unwrap();
    for serial in [None, Some("same-advertised-string")] {
        let mut topology = Topology::new();
        for product in [0x0064, 0x00c8, 0x0064, 0x00c8] {
            topology
                .add_physical_device(
                    None,
                    spec(
                        Some(UsbIds {
                            vendor: 0x37d1,
                            product,
                        }),
                        serial,
                        vec![serial_channel(None)],
                    ),
                )
                .unwrap();
        }
        for physical in topology.physical_devices() {
            let result = registry.inspect(&topology, physical.id(), &[]).unwrap();
            assert!(matches!(
                result.identification,
                Identification::Identified(_)
            ));
            assert_eq!(result.matches[0].plan.keyboard, physical.keyboard());
            assert_eq!(
                physical.identity().absence(),
                Some(IdentityAbsence::NotReported)
            );
            assert_eq!(topology.members(physical.keyboard()).count(), 1);
        }
        assert_eq!(topology.keyboards().count(), 4);
    }
}

#[test]
fn matching_preserves_existing_capability_evidence_and_plans_require_explicit_acceptance() {
    let mut topology = Topology::new();
    let physical = topology
        .add_physical_device(None, spec(Some(USB), None, vec![serial_channel(None)]))
        .unwrap();
    let source = topology.endpoints(physical).next().unwrap().id();
    topology.revalidate_endpoint(source).unwrap();
    let keyboard = topology.physical_device(physical).unwrap().keyboard();
    let scope = CapabilityScope::Keyboard(keyboard);
    let token = topology.begin_capability_refresh(scope, source).unwrap();
    topology
        .complete_capability_refresh(
            token,
            CapabilityReport::new(
                "test-reporter",
                vec![],
                vec![(
                    CapabilityOperation::ReadBattery,
                    CapabilityEntry::new(Support::Supported, vec![]),
                )],
            )
            .unwrap(),
        )
        .unwrap();
    let before = format!("{topology:?}");
    let backend = StubBackend::new(Behavior::Match);
    let result = registry(vec![definition("known", serial_channel(None))])
        .inspect(&topology, physical, &[&backend])
        .unwrap();
    assert_eq!(format!("{topology:?}"), before);
    let plan = &result.matches[0].plan;
    assert_eq!(plan.components.len(), 1);
    assert_eq!(topology.components(physical).count(), 0);
    // Explicit host acceptance uses existing topology operations, which also
    // invalidate old capability context. The registry itself never applies it.
    let component = topology
        .add_component(physical, None, plan.components[0].kind, None)
        .unwrap();
    let route = result.actionable().unwrap();
    let context = ConnectContext::new(
        &topology,
        route.primary,
        route.candidate.endpoints().to_vec(),
        vec![(component, route.primary)],
    )
    .unwrap();
    assert_eq!(context.keyboard(), keyboard);
    assert_eq!(backend.connects.load(Ordering::Relaxed), 0);
}

fn two_roles(first: EndpointSelector, second: EndpointSelector) -> DeviceDefinition {
    let mut definition = definition("two-roles", first.clone());
    definition.protocols[0].primary_role = "first".into();
    definition.protocols[0].roles = vec![
        EndpointRole {
            key: "first".into(),
            selector: first,
        },
        EndpointRole {
            key: "second".into(),
            selector: second,
        },
    ];
    definition
}

#[test]
fn distinct_endpoint_constraints_can_prove_one_assignment_even_with_an_initially_broad_role() {
    let mut topology = Topology::new();
    let physical = topology
        .add_physical_device(
            None,
            spec(
                Some(USB),
                None,
                vec![serial_channel(Some(0)), serial_channel(Some(1))],
            ),
        )
        .unwrap();
    let definition = two_roles(serial_channel(None), serial_channel(Some(1)));
    let backend = StubBackend::new(Behavior::Match);
    let result = registry(vec![definition])
        .inspect(&topology, physical, &[&backend])
        .unwrap();
    let ids: Vec<_> = topology
        .endpoints(physical)
        .map(|endpoint| endpoint.id())
        .collect();
    let route = result.actionable().unwrap();
    assert_eq!(
        route.roles,
        BTreeMap::from([("first".into(), ids[0]), ("second".into(), ids[1])])
    );
    assert_eq!(route.candidate.endpoints(), ids);
    assert_eq!(route.primary, ids[0]);
}

#[test]
fn endpoint_ambiguity_and_exhausted_search_never_select_the_first_assignment() {
    let mut topology = Topology::new();
    let physical = topology
        .add_physical_device(
            None,
            spec(
                Some(USB),
                None,
                vec![serial_channel(Some(0)), serial_channel(Some(1))],
            ),
        )
        .unwrap();
    let backend = StubBackend::new(Behavior::Match);
    let ambiguous = registry(vec![two_roles(serial_channel(None), serial_channel(None))])
        .inspect(&topology, physical, &[&backend])
        .unwrap();
    assert!(matches!(
        ambiguous.matches[0].protocols[0].assignment,
        Assignment::Ambiguous { .. }
    ));
    assert_eq!(
        ambiguous.matches[0].protocols[0].routing,
        RouteStatus::Unresolved
    );
    assert!(ambiguous.routes.is_empty() && ambiguous.actionable().is_none());
    // This budget finds a complete assignment, but cannot prove that no second
    // assignment exists. Reporting it as unique would make budget a selection policy.
    let limited = Registry::new(
        vec![two_roles(serial_channel(None), serial_channel(Some(1)))],
        sources(),
        2,
    )
    .unwrap()
    .inspect(&topology, physical, &[&backend])
    .unwrap();
    assert_eq!(
        limited.matches[0].protocols[0].assignment,
        Assignment::Indeterminate
    );
    assert_eq!(
        limited.matches[0].protocols[0].routing,
        RouteStatus::Unresolved
    );
    assert!(limited.actionable().is_none());
    assert_eq!(backend.matches.load(Ordering::Relaxed), 0);
}

#[test]
fn one_endpoint_cannot_be_reused_to_satisfy_two_required_roles() {
    let mut topology = Topology::new();
    let physical = topology
        .add_physical_device(None, spec(Some(USB), None, vec![serial_channel(Some(0))]))
        .unwrap();
    let result = registry(vec![two_roles(
        serial_channel(Some(0)),
        serial_channel(Some(0)),
    )])
    .inspect(&topology, physical, &[])
    .unwrap();
    let protocol = &result.matches[0].protocols[0];
    assert!(protocol.roles.iter().all(|role| role.matches.len() == 1));
    assert_eq!(protocol.assignment, Assignment::NoMatch);
}

#[test]
fn different_named_role_assignments_remain_distinct_when_ownership_endpoint_sets_are_identical() {
    let mut topology = Topology::new();
    let physical = topology
        .add_physical_device(
            None,
            spec(
                Some(USB),
                None,
                vec![serial_channel(Some(0)), serial_channel(Some(1))],
            ),
        )
        .unwrap();
    let mut definition = two_roles(serial_channel(Some(0)), serial_channel(Some(1)));
    let mut alternative = definition.protocols[0].clone();
    alternative.id = "alternative".into();
    alternative.roles[0].selector.interface_number = Some(1);
    alternative.roles[1].selector.interface_number = Some(0);
    definition.protocols.push(alternative);
    let backend = StubBackend::new(Behavior::Match);
    let result = registry(vec![definition])
        .inspect(&topology, physical, &[&backend])
        .unwrap();
    assert!(matches!(
        result.identification,
        Identification::Identified(_)
    ));
    assert_eq!(result.routes.len(), 2);
    assert_eq!(result.routes[0].candidate, result.routes[1].candidate);
    assert_ne!(result.routes[0].roles, result.routes[1].roles);
    assert!(result.actionable().is_none());
}

#[test]
fn a_complete_route_does_not_hide_an_unresolved_registered_alternative() {
    let mut topology = Topology::new();
    let physical = topology
        .add_physical_device(
            None,
            spec(
                Some(USB),
                None,
                vec![serial_channel(Some(0)), serial_channel(Some(1))],
            ),
        )
        .unwrap();
    let mut definition = definition("one-device", serial_channel(Some(0)));
    let mut broad = definition.protocols[0].clone();
    broad.id = "broad".into();
    broad.roles[0].selector.interface_number = None;
    definition.protocols.push(broad);
    let backend = StubBackend::new(Behavior::Match);
    let result = registry(vec![definition])
        .inspect(&topology, physical, &[&backend])
        .unwrap();
    assert_eq!(result.routes.len(), 1);
    assert!(result.actionable().is_none());
}

#[test]
fn multiple_gmmk_interfaces_keep_one_physical_and_logical_identity_without_inventing_a_management_selector()
 {
    let mut topology = Topology::new();
    let physical = topology
        .add_physical_device(
            None,
            spec(
                Some(UsbIds {
                    vendor: 0x320f,
                    product: 0x5044,
                }),
                None,
                vec![
                    hid_channel(Some(0), None, None),
                    hid_channel(Some(1), None, None),
                ],
            ),
        )
        .unwrap();
    let result = builtin::registry()
        .unwrap()
        .inspect(&topology, physical, &[])
        .unwrap();
    assert_eq!(
        result.identification,
        Identification::Identified("glorious.gmmk-pro.rev1-ansi".into())
    );
    assert_eq!(result.matches[0].plan.interfaces.len(), 2);
    assert_eq!(result.matches[0].plan.endpoints.len(), 2);
    assert_eq!(topology.members(result.matches[0].plan.keyboard).count(), 1);
    assert_eq!(topology.keyboards().count(), 1);
    assert!(result.matches[0].protocols.is_empty() && result.routes.is_empty());
}

#[test]
fn malformed_registry_declarations_are_rejected_before_any_matching() {
    for case in 0..10 {
        let mut definition = definition("bad", serial_channel(None));
        match case {
            0 => definition.sources = vec!["missing".into()],
            1 => definition.components[0].parent = Some("missing".into()),
            2 => definition.components[0].parent = Some("body".into()),
            3 => {
                let repeated = definition.components[0].clone();
                definition.components.push(repeated);
            }
            4 => definition.protocols[0].primary_role = "missing".into(),
            5 => definition.protocols[0].roles[0].selector.hid.usage = Some(0),
            6 => definition.mode = DeviceMode::Bootloader,
            7 => {
                let repeated = definition.protocols[0].clone();
                definition.protocols.push(repeated);
            }
            8 => {
                let repeated = definition.protocols[0].roles[0].clone();
                definition.protocols[0].roles.push(repeated);
            }
            _ => definition.protocols[0].roles.clear(),
        }
        assert!(matches!(
            Registry::new(vec![definition], sources(), 4096),
            Err(RegistryError::InvalidDefinition { .. })
        ));
    }
    let definition = definition("duplicate", serial_channel(None));
    assert!(Registry::new(vec![definition.clone(), definition], sources(), 4096).is_err());
    let mut repeated_sources = sources();
    repeated_sources.push(repeated_sources[0].clone());
    assert!(Registry::new(vec![], repeated_sources, 4096).is_err());
    assert!(Registry::new(vec![], sources(), 0).is_err());
    assert!(Registry::new(vec![], sources(), usize::MAX).is_err());
}

#[test]
fn contradictory_model_transport_metadata_fails_without_publishing_partial_hardware() {
    let mut topology = Topology::new();
    let mut unit = spec(Some(USB), None, vec![serial_channel(None)]);
    unit.endpoints[0].hid.usage_page = Some(0);
    assert!(matches!(
        topology.add_physical_device(None, unit),
        Err(crate::model::ModelError::UnexpectedHidMetadata { index: 0 })
    ));
    assert_eq!(topology.physical_devices().count(), 0);
    assert_eq!(topology.keyboards().count(), 0);
}
