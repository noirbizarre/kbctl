use super::{
    ModelError,
    connection::{ConnectionState, EndpointLocator, TransportKind, UnavailableReason},
    identity::{
        HardwareIdentity, IdentityAbsence, IdentityClaim, IdentityComparison, IdentityScope,
        Verification,
    },
    topology::{
        ComponentKind, ComponentSpec, DisplayName, EndpointSpec, IdentityCandidates, InterfaceSpec,
        PhysicalDeviceSpec, Side, Topology, UsbIds,
    },
};

fn name(value: &str) -> DisplayName {
    DisplayName::new(value).unwrap()
}

fn claim(
    namespace: &str,
    value: &str,
    scope: IdentityScope,
    verification: Verification,
) -> IdentityClaim {
    IdentityClaim::new(namespace, value, scope, verification).unwrap()
}

fn verified(value: &str) -> HardwareIdentity {
    HardwareIdentity::from_claims(vec![claim(
        "test.vendor/unit",
        value,
        IdentityScope::Unit,
        Verification::Verified,
    )])
    .unwrap()
}

fn component(kind: ComponentKind, parent: Option<usize>) -> ComponentSpec {
    ComponentSpec {
        name: None,
        kind,
        parent,
    }
}

/// Synthetic discovery data exercises relationships, not a real device protocol.
fn unit(label: &str) -> PhysicalDeviceSpec {
    PhysicalDeviceSpec {
        name: Some(name(label)),
        identity: HardwareIdentity::missing(IdentityAbsence::NotReported),
        usb_ids: Some(UsbIds {
            vendor: 0x1234,
            product: 0x5678,
        }),
        interfaces: vec![],
        serial: None,
        endpoints: vec![EndpointSpec {
            interface: None,
            transport: TransportKind::Serial,
            locator: EndpointLocator::new("/dev/example").unwrap(),
            hid: Default::default(),
        }],
        components: vec![component(ComponentKind::Body, None)],
    }
}

#[test]
fn multiple_interfaces_of_a_gmmk_pro_form_one_logical_keyboard() {
    let mut topology = Topology::new();
    let mut spec = unit("GMMK Pro");
    spec.interfaces = vec![
        InterfaceSpec {
            name: None,
            usb_number: Some(0),
        },
        InterfaceSpec {
            name: None,
            usb_number: Some(1),
        },
    ];
    spec.endpoints = (0..2)
        .map(|index| EndpointSpec {
            interface: Some(index),
            transport: TransportKind::Hid,
            locator: EndpointLocator::new(format!("/dev/hidraw{index}")).unwrap(),
            hid: Default::default(),
        })
        .collect();
    let physical = topology.add_physical_device(None, spec).unwrap();
    let keyboard = topology.physical_device(physical).unwrap().keyboard();
    assert_eq!(topology.keyboards().count(), 1);
    assert_eq!(topology.members(keyboard).count(), 1);
    assert_eq!(topology.interfaces(physical).count(), 2);
    assert_eq!(topology.components(physical).count(), 1);
    let interfaces: Vec<_> = topology.interfaces(physical).map(|i| i.id()).collect();
    for (index, endpoint) in topology.endpoints(physical).enumerate() {
        assert_eq!(endpoint.physical(), physical);
        assert_eq!(endpoint.interface(), Some(interfaces[index]));
        assert_eq!(endpoint.transport(), TransportKind::Hid);
        assert_eq!(endpoint.state(), ConnectionState::Unvalidated);
    }
}

#[test]
fn one_defy_controller_channel_can_serve_both_hidden_halves() {
    let mut topology = Topology::new();
    let mut spec = unit("Dygma Defy Neuron");
    spec.components = vec![
        component(ComponentKind::Controller, None),
        component(ComponentKind::Half(Some(Side::Left)), Some(0)),
        component(ComponentKind::Half(Some(Side::Right)), Some(0)),
    ];
    let physical = topology.add_physical_device(None, spec).unwrap();
    let endpoint = topology.endpoints(physical).next().unwrap().id();
    let components: Vec<_> = topology.components(physical).map(|c| c.id()).collect();
    topology.revalidate_endpoint(endpoint).unwrap();
    assert_eq!(topology.physical_devices().count(), 1);
    assert_eq!(topology.endpoints(physical).count(), 1);
    for half in &components[1..] {
        assert_eq!(
            topology.component(*half).unwrap().parent(),
            Some(components[0])
        );
        let token = topology.begin_operation(endpoint, *half).unwrap();
        assert_eq!(topology.validate_completion(token), Ok(()));
    }
}

#[test]
fn removing_a_naya_module_preserves_both_physical_halves_and_other_modules() {
    let mut topology = Topology::new();
    let mut left_spec = unit("Naya Create left");
    left_spec.components = vec![
        component(ComponentKind::Half(Some(Side::Left)), None),
        component(ComponentKind::Module, Some(0)),
        component(ComponentKind::Module, Some(1)),
        component(ComponentKind::Module, Some(0)),
    ];
    let left = topology.add_physical_device(None, left_spec).unwrap();
    let keyboard = topology.physical_device(left).unwrap().keyboard();
    let mut right_spec = unit("Naya Create right");
    right_spec.components = vec![component(ComponentKind::Half(Some(Side::Right)), None)];
    let right = topology
        .add_physical_device(Some(keyboard), right_spec)
        .unwrap();
    let components: Vec<_> = topology.components(left).map(|c| c.id()).collect();
    topology.remove_component(components[1]).unwrap();
    assert!(topology.component(components[1]).is_err());
    assert!(topology.component(components[2]).is_err());
    assert!(topology.component(components[0]).is_ok());
    assert!(topology.component(components[3]).is_ok());
    assert_eq!(topology.members(keyboard).count(), 2);
    assert_eq!(topology.endpoints(right).count(), 1);
    assert_eq!(topology.components(right).count(), 1);
}

#[test]
fn similar_halves_and_duplicate_identity_claims_remain_separate_and_ambiguous() {
    let mut topology = Topology::new();
    for side in [Side::Left, Side::Right, Side::Left, Side::Right] {
        let mut spec = unit("Create");
        spec.components = vec![component(ComponentKind::Half(Some(side)), None)];
        spec.identity = verified("broken-duplicate-serial");
        topology.add_physical_device(None, spec).unwrap();
    }
    assert_eq!(topology.keyboards().count(), 4);
    let IdentityCandidates::Ambiguous(candidates) =
        topology.identity_candidates(&verified("broken-duplicate-serial"))
    else {
        panic!("duplicate claims must not identify one unit");
    };
    assert_eq!(candidates.len(), 4);
}

#[test]
fn hardware_without_stable_identifiers_or_usable_channels_is_representable() {
    let mut topology = Topology::new();
    let mut spec = unit("Unknown hardware");
    spec.name = None;
    spec.endpoints.clear();
    spec.components.clear();
    let physical = topology.add_physical_device(None, spec).unwrap();
    assert_eq!(topology.endpoints(physical).count(), 0);
    assert_eq!(topology.interfaces(physical).count(), 0);
    assert_eq!(topology.components(physical).count(), 0);
    let device = topology.physical_device(physical).unwrap();
    assert_eq!(device.name().as_str(), "Unknown physical device");
    assert_eq!(
        topology
            .keyboard(device.keyboard())
            .unwrap()
            .name()
            .as_str(),
        "Unknown keyboard"
    );
    assert_eq!(
        device.identity().absence(),
        Some(IdentityAbsence::NotReported)
    );
    assert_eq!(
        topology.identity_candidates(device.identity()),
        IdentityCandidates::InsufficientEvidence
    );
}

#[test]
fn invalid_specifications_do_not_publish_partial_records() {
    let mut topology = Topology::new();
    let physical = topology
        .add_physical_device(None, unit("existing"))
        .unwrap();
    let keyboard = topology.physical_device(physical).unwrap().keyboard();
    for invalid in 0..5 {
        let mut spec = unit("invalid");
        match invalid {
            0 => spec.endpoints[0].interface = Some(0),
            1 => spec.components[0].parent = Some(2),
            2 => spec.components[0].parent = Some(0),
            3 => {
                spec.components = vec![
                    component(ComponentKind::Module, Some(1)),
                    component(ComponentKind::Module, Some(0)),
                ]
            }
            _ => {
                spec.interfaces = vec![
                    InterfaceSpec {
                        name: None,
                        usb_number: Some(1),
                    },
                    InterfaceSpec {
                        name: None,
                        usb_number: Some(1),
                    },
                ]
            }
        }
        assert!(topology.add_physical_device(Some(keyboard), spec).is_err());
        assert_eq!(topology.physical_devices().count(), 1);
        assert_eq!(topology.keyboards().count(), 1);
        assert_eq!(topology.components(physical).count(), 1);
        assert_eq!(topology.endpoints(physical).count(), 1);
    }
}

#[test]
fn interface_indexes_cannot_reference_another_physical_device() {
    let mut topology = Topology::new();
    let mut first = unit("first");
    first.interfaces.push(InterfaceSpec {
        name: None,
        usb_number: Some(0),
    });
    topology.add_physical_device(None, first).unwrap();
    let mut second = unit("second");
    second.endpoints[0].interface = Some(0);
    assert!(matches!(
        topology.add_physical_device(None, second),
        Err(ModelError::InvalidReference {
            kind: "interface",
            index: 0
        })
    ));
    assert_eq!(topology.physical_devices().count(), 1);
}

#[test]
fn component_parents_must_belong_to_the_same_physical_device() {
    let mut topology = Topology::new();
    let first = topology.add_physical_device(None, unit("first")).unwrap();
    let keyboard = topology.physical_device(first).unwrap().keyboard();
    let second = topology
        .add_physical_device(Some(keyboard), unit("second"))
        .unwrap();
    let parent = topology.components(first).next().unwrap().id();
    assert_eq!(
        topology.add_component(second, Some(parent), ComponentKind::Module, None),
        Err(ModelError::CrossOwner {
            component: parent,
            physical: second
        })
    );
    assert_eq!(topology.components(second).count(), 1);
    let other = topology
        .add_physical_device(None, unit("another keyboard"))
        .unwrap();
    assert!(matches!(
        topology.add_component(other, Some(parent), ComponentKind::Module, None),
        Err(ModelError::CrossOwner { .. })
    ));
    topology.remove_component(parent).unwrap();
    assert_eq!(
        topology.add_component(first, Some(parent), ComponentKind::Module, None),
        Err(ModelError::UnknownComponent(parent))
    );
}

#[test]
fn explicit_association_retires_only_the_empty_keyboard_shell() {
    let mut topology = Topology::new();
    let first = topology.add_physical_device(None, unit("left")).unwrap();
    let second = topology.add_physical_device(None, unit("right")).unwrap();
    let old_keyboard = topology.physical_device(second).unwrap().keyboard();
    let target = topology.physical_device(first).unwrap().keyboard();
    topology.associate_physical_device(second, target).unwrap();
    assert_eq!(topology.members(target).count(), 2);
    assert_eq!(topology.keyboards().count(), 1);
    assert_eq!(
        topology.keyboard(old_keyboard),
        Err(ModelError::UnknownKeyboard(old_keyboard))
    );
    assert!(topology.components(second).all(|c| c.keyboard() == target));
    assert!(matches!(
        topology.associate_physical_device(second, old_keyboard),
        Err(ModelError::UnknownKeyboard(_))
    ));
    assert_eq!(topology.physical_device(second).unwrap().keyboard(), target);
    assert!(matches!(
        topology.add_physical_device(Some(old_keyboard), unit("invalid")),
        Err(ModelError::UnknownKeyboard(_))
    ));
    assert_eq!(topology.members(target).count(), 2);
}

#[test]
fn retiring_one_physical_member_preserves_the_keyboard_until_its_last_member_leaves() {
    let mut topology = Topology::new();
    let first = topology.add_physical_device(None, unit("left")).unwrap();
    let keyboard = topology.physical_device(first).unwrap().keyboard();
    let second = topology
        .add_physical_device(Some(keyboard), unit("right"))
        .unwrap();
    topology.retire_physical_device(first).unwrap();
    assert_eq!(topology.members(keyboard).count(), 1);
    assert_eq!(topology.components(first).count(), 0);
    assert_eq!(topology.endpoints(first).count(), 0);
    assert_eq!(topology.components(second).count(), 1);
    topology.retire_physical_device(second).unwrap();
    assert_eq!(topology.keyboards().count(), 0);
    assert_eq!(
        topology.retire_physical_device(first),
        Err(ModelError::UnknownPhysicalDevice(first))
    );
}

#[test]
fn reconnect_matches_hardware_evidence_but_never_reuses_runtime_instances() {
    let mut topology = Topology::new();
    let mut spec = unit("keyboard");
    spec.identity = verified("unit-42");
    spec.interfaces.push(InterfaceSpec {
        name: None,
        usb_number: Some(0),
    });
    let physical = topology.add_physical_device(None, spec).unwrap();
    let keyboard = topology.physical_device(physical).unwrap().keyboard();
    let interface = topology.interfaces(physical).next().unwrap().id();
    let endpoint = topology.endpoints(physical).next().unwrap().id();
    let component = topology.components(physical).next().unwrap().id();
    topology.revalidate_endpoint(endpoint).unwrap();
    let token = topology.begin_operation(endpoint, component).unwrap();
    let evidence = topology
        .physical_device(physical)
        .unwrap()
        .identity()
        .clone();
    topology.retire_physical_device(physical).unwrap();
    let mut spec = unit("renamed by discovery");
    spec.identity = verified("unit-42");
    spec.interfaces.push(InterfaceSpec {
        name: None,
        usb_number: Some(0),
    });
    let rediscovered = topology.add_physical_device(None, spec).unwrap();
    assert_ne!(physical, rediscovered);
    assert_ne!(
        keyboard,
        topology.physical_device(rediscovered).unwrap().keyboard()
    );
    assert_ne!(
        interface,
        topology.interfaces(rediscovered).next().unwrap().id()
    );
    assert_ne!(
        endpoint,
        topology.endpoints(rediscovered).next().unwrap().id()
    );
    assert_ne!(
        component,
        topology.components(rediscovered).next().unwrap().id()
    );
    assert_eq!(
        evidence.compare(topology.physical_device(rediscovered).unwrap().identity()),
        IdentityComparison::Matching
    );
    assert_eq!(
        topology.identity_candidates(&evidence),
        IdentityCandidates::One(rediscovered)
    );
    assert!(matches!(
        topology.validate_completion(token),
        Err(ModelError::StaleOperation { .. })
    ));
    let mut another = Topology::new();
    assert_ne!(
        rediscovered,
        another
            .add_physical_device(None, unit("new topology"))
            .unwrap()
    );
}

#[test]
fn access_invalidation_and_revalidation_reject_old_work_without_retiring_hardware() {
    let mut topology = Topology::new();
    let physical = topology
        .add_physical_device(None, unit("keyboard"))
        .unwrap();
    let endpoint = topology.endpoints(physical).next().unwrap().id();
    let component = topology.components(physical).next().unwrap().id();
    assert_eq!(
        topology.begin_operation(endpoint, component),
        Err(ModelError::EndpointNotReady(endpoint))
    );
    let generation = topology.revalidate_endpoint(endpoint).unwrap();
    let token = topology.begin_operation(endpoint, component).unwrap();
    for reason in [UnavailableReason::PermissionDenied, UnavailableReason::Busy] {
        topology.invalidate_endpoint(endpoint, reason).unwrap();
        assert_eq!(
            topology.endpoint(endpoint).unwrap().state(),
            ConnectionState::Unavailable(reason)
        );
        assert!(topology.validate_completion(token).is_err());
        assert_eq!(topology.physical_devices().count(), 1);
        assert_eq!(topology.components(physical).count(), 1);
    }
    assert_ne!(generation, topology.revalidate_endpoint(endpoint).unwrap());
    assert!(topology.validate_completion(token).is_err());
    let fresh = topology.begin_operation(endpoint, component).unwrap();
    assert_eq!(topology.validate_completion(fresh), Ok(()));
    topology.revalidate_endpoint(endpoint).unwrap();
    assert!(topology.validate_completion(fresh).is_err());
}

#[test]
fn component_replacement_and_membership_changes_invalidate_previous_results() {
    let mut topology = Topology::new();
    let physical = topology.add_physical_device(None, unit("first")).unwrap();
    let endpoint = topology.endpoints(physical).next().unwrap().id();
    let parent = topology.components(physical).next().unwrap().id();
    let module = topology
        .add_component(physical, Some(parent), ComponentKind::Module, None)
        .unwrap();
    topology.revalidate_endpoint(endpoint).unwrap();
    let token = topology.begin_operation(endpoint, module).unwrap();
    topology.remove_component(module).unwrap();
    let replacement = topology
        .add_component(physical, Some(parent), ComponentKind::Module, None)
        .unwrap();
    assert_ne!(module, replacement);
    assert!(topology.validate_completion(token).is_err());
    let token = topology.begin_operation(endpoint, replacement).unwrap();
    let other = topology.add_physical_device(None, unit("second")).unwrap();
    let target = topology.components(other).next().unwrap().id();
    assert!(matches!(
        topology.begin_operation(endpoint, target),
        Err(ModelError::CrossKeyboard { .. })
    ));
    let keyboard = topology.physical_device(other).unwrap().keyboard();
    topology
        .associate_physical_device(physical, keyboard)
        .unwrap();
    assert!(topology.validate_completion(token).is_err());
    assert!(topology.begin_operation(endpoint, target).is_ok());
}

#[test]
fn returning_to_the_original_keyboard_does_not_revive_stale_operations() {
    let mut topology = Topology::new();
    let first = topology
        .add_physical_device(None, unit("stationary"))
        .unwrap();
    let original = topology.physical_device(first).unwrap().keyboard();
    let moving = topology
        .add_physical_device(Some(original), unit("moving"))
        .unwrap();
    let other = topology
        .add_physical_device(None, unit("destination"))
        .unwrap();
    let destination = topology.physical_device(other).unwrap().keyboard();
    let stationary_endpoint = topology.endpoints(first).next().unwrap().id();
    let moving_endpoint = topology.endpoints(moving).next().unwrap().id();
    let component = topology.components(moving).next().unwrap().id();
    topology.revalidate_endpoint(stationary_endpoint).unwrap();
    topology.revalidate_endpoint(moving_endpoint).unwrap();
    let remote = topology
        .begin_operation(stationary_endpoint, component)
        .unwrap();
    let local = topology
        .begin_operation(moving_endpoint, component)
        .unwrap();
    topology
        .associate_physical_device(moving, original)
        .unwrap();
    assert_eq!(topology.validate_completion(local), Ok(()));
    topology
        .associate_physical_device(moving, destination)
        .unwrap();
    topology
        .associate_physical_device(moving, original)
        .unwrap();
    assert!(topology.validate_completion(remote).is_err());
    assert!(topology.validate_completion(local).is_err());
    assert!(
        topology
            .begin_operation(stationary_endpoint, component)
            .is_ok()
    );
}

#[test]
fn identity_matching_requires_compatible_verified_unit_evidence() {
    let missing = HardwareIdentity::missing(IdentityAbsence::NotReported);
    assert_eq!(missing.compare(&missing), IdentityComparison::Insufficient);
    assert_eq!(
        verified("a").compare(&verified("b")),
        IdentityComparison::Conflicting
    );
    let advertised = HardwareIdentity::from_claims(vec![claim(
        "test.vendor/unit",
        "a",
        IdentityScope::Unit,
        Verification::Advertised,
    )])
    .unwrap();
    let group = HardwareIdentity::from_claims(vec![claim(
        "test.vendor/unit",
        "a",
        IdentityScope::Group,
        Verification::Verified,
    )])
    .unwrap();
    let unrelated = HardwareIdentity::from_claims(vec![claim(
        "another.issuer/unit",
        "a",
        IdentityScope::Unit,
        Verification::Verified,
    )])
    .unwrap();
    for insufficient in [&missing, &advertised, &group, &unrelated] {
        assert_eq!(
            verified("a").compare(insufficient),
            IdentityComparison::Insufficient
        );
    }
    let conflicting = HardwareIdentity::from_claims(vec![
        claim(
            "test.vendor/unit",
            "a",
            IdentityScope::Unit,
            Verification::Verified,
        ),
        claim(
            "test.vendor/alternate-unit",
            "different",
            IdentityScope::Unit,
            Verification::Verified,
        ),
    ])
    .unwrap();
    let original = HardwareIdentity::from_claims(vec![
        claim(
            "test.vendor/unit",
            "a",
            IdentityScope::Unit,
            Verification::Verified,
        ),
        claim(
            "test.vendor/alternate-unit",
            "original",
            IdentityScope::Unit,
            Verification::Verified,
        ),
    ])
    .unwrap();
    assert_eq!(
        original.compare(&conflicting),
        IdentityComparison::Conflicting
    );
    assert_eq!(
        conflicting.compare(&original),
        IdentityComparison::Conflicting
    );
    assert_eq!(
        Topology::new().identity_candidates(&verified("absent")),
        IdentityCandidates::NoMatch
    );
}

#[test]
fn malformed_identity_claims_and_blank_display_metadata_are_rejected() {
    assert!(IdentityClaim::new(" ", "value", IdentityScope::Unit, Verification::Verified).is_err());
    assert!(
        IdentityClaim::new("issuer", "\n", IdentityScope::Unit, Verification::Verified).is_err()
    );
    assert!(HardwareIdentity::from_claims(vec![]).is_err());
    assert!(matches!(
        HardwareIdentity::from_claims(vec![
            claim("issuer", "a", IdentityScope::Unit, Verification::Verified),
            claim("issuer", "b", IdentityScope::Unit, Verification::Verified)
        ]),
        Err(ModelError::DuplicateClaim { .. })
    ));
    assert!(DisplayName::new(" ").is_err());
    assert!(EndpointLocator::new("").is_err());
    // Preserve exact values rather than normalizing unrelated hardware claims.
    assert_eq!(
        verified(" a ").compare(&verified("a")),
        IdentityComparison::Conflicting
    );
}

#[test]
fn human_readable_names_do_not_change_instance_or_hardware_identity() {
    let mut topology = Topology::new();
    let mut spec = unit("keyboard");
    spec.identity = verified("a");
    let physical = topology.add_physical_device(None, spec).unwrap();
    let keyboard = topology.physical_device(physical).unwrap().keyboard();
    topology
        .rename_keyboard(keyboard, name("Desk keyboard"))
        .unwrap();
    topology
        .rename_physical_device(physical, name("USB controller"))
        .unwrap();
    assert_eq!(topology.keyboard(keyboard).unwrap().id(), keyboard);
    assert_eq!(
        topology.keyboard(keyboard).unwrap().name().as_str(),
        "Desk keyboard"
    );
    assert_eq!(
        topology.physical_device(physical).unwrap().name().as_str(),
        "USB controller"
    );
    assert_eq!(
        topology.identity_candidates(&verified("a")),
        IdentityCandidates::One(physical)
    );
}
