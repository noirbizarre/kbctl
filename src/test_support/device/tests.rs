use super::*;
use crate::model::{
    connection::{EndpointLocator, TransportKind},
    identity::{HardwareIdentity, IdentityAbsence},
    topology::{
        ComponentKind, ComponentSpec, EndpointSpec, HidMetadata, InterfaceSpec, PhysicalDeviceSpec,
        Side,
    },
};

fn part(alias: &str, group: &str) -> DevicePart {
    DevicePart {
        alias: alias.into(),
        group: group.into(),
        interfaces: vec![],
        endpoints: vec![format!("{alias}-port")],
        components: vec![format!("{alias}-half")],
        spec: PhysicalDeviceSpec {
            name: None,
            identity: HardwareIdentity::missing(IdentityAbsence::NotReported),
            usb_ids: None,
            serial: None,
            interfaces: vec![],
            endpoints: vec![EndpointSpec {
                interface: None,
                transport: TransportKind::Serial,
                locator: EndpointLocator::new("synthetic").unwrap(),
                hid: Default::default(),
            }],
            components: vec![ComponentSpec {
                name: None,
                kind: ComponentKind::Half(Some(Side::Left)),
                parent: None,
            }],
        },
    }
}

#[test]
fn explicit_split_groups_preserve_physical_members_and_authorized_routes() {
    let left = part("left", "pair");
    let mut right = part("right", "pair");
    right.spec.components[0].kind = ComponentKind::Half(Some(Side::Right));
    let mut fixture = DeviceBuilder::new()
        .physical(left)
        .physical(right)
        .route("left-half", "left-port")
        .route("right-half", "right-port")
        .build()
        .unwrap();
    assert_eq!(fixture.topology.keyboards().count(), 1);
    assert_eq!(fixture.topology.members(fixture.groups["pair"]).count(), 2);
    assert!(
        fixture
            .context(&fixture.topology, "left-port", &["left-port", "right-port"])
            .is_err()
    );
    fixture.revalidate().unwrap();
    let context = fixture
        .context(&fixture.topology, "left-port", &["right-port", "left-port"])
        .unwrap();
    assert_eq!(context.component_routes().count(), 2);
}

#[test]
fn controller_children_and_hid_collections_are_not_additional_physical_keyboards() {
    let mut controller = part("controller", "keyboard");
    controller.components = vec![
        "controller-root".into(),
        "hidden-left".into(),
        "hidden-module".into(),
    ];
    controller.spec.components = vec![
        ComponentSpec {
            name: None,
            kind: ComponentKind::Controller,
            parent: None,
        },
        ComponentSpec {
            name: None,
            kind: ComponentKind::Half(Some(Side::Left)),
            parent: Some(0),
        },
        ComponentSpec {
            name: None,
            kind: ComponentKind::Module,
            parent: Some(1),
        },
    ];
    controller.interfaces = vec!["hid-interface".into()];
    controller.spec.interfaces = vec![InterfaceSpec {
        name: None,
        usb_number: Some(3),
    }];
    controller.endpoints = vec!["collection-a".into(), "collection-b".into()];
    controller.spec.endpoints = (1..=2)
        .map(|usage| EndpointSpec {
            interface: Some(0),
            transport: TransportKind::Hid,
            locator: EndpointLocator::new(format!("collection-{usage}")).unwrap(),
            hid: HidMetadata {
                usage_page: Some(0xff00),
                usage: Some(usage),
            },
        })
        .collect();
    let fixture = DeviceBuilder::new()
        .physical(controller)
        .route("hidden-left", "collection-a")
        .route("hidden-module", "collection-a")
        .build()
        .unwrap();
    assert_eq!(fixture.topology.physical_devices().count(), 1);
    assert_eq!(fixture.topology.keyboards().count(), 1);
    assert_eq!(
        fixture
            .topology
            .component(fixture.components["hidden-module"])
            .unwrap()
            .parent(),
        Some(fixture.components["hidden-left"])
    );
    assert_ne!(
        fixture
            .topology
            .endpoint(fixture.endpoints["collection-a"])
            .unwrap()
            .hid(),
        fixture
            .topology
            .endpoint(fixture.endpoints["collection-b"])
            .unwrap()
            .hid()
    );
}

#[test]
fn bad_aliases_counts_and_cross_keyboard_routes_fail_construction() {
    let left = part("left", "one");
    let right = part("right", "two");
    assert!(
        DeviceBuilder::new()
            .physical(left)
            .physical(right)
            .route("left-half", "right-port")
            .build()
            .is_err()
    );
    let mut broken = part("bad", "one");
    broken.endpoints.clear();
    assert!(DeviceBuilder::new().physical(broken).build().is_err());
    assert!(
        DeviceBuilder::new()
            .physical(part("same", "one"))
            .physical(part("same", "two"))
            .build()
            .is_err()
    );
    assert!(
        DeviceBuilder::new()
            .physical(part("unit", "one"))
            .route("missing", "unit-port")
            .build()
            .is_err()
    );
}

#[test]
fn discovery_only_fixtures_need_no_primary_or_open_channel() {
    let mut unit = part("unknown", "unknown");
    unit.spec.endpoints.clear();
    unit.endpoints.clear();
    unit.spec.components.clear();
    unit.components.clear();
    let fixture = DeviceBuilder::new().physical(unit).build().unwrap();
    assert_eq!(fixture.topology.physical_devices().count(), 1);
    assert!(fixture.context(&fixture.topology, "missing", &[]).is_err());
}
