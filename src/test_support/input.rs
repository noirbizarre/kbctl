//! Synthetic input metadata, never a claim about attached hardware.
use crate::model::{identity::Identification, input::*, topology::UsbIds};
use std::collections::BTreeMap;

pub(crate) fn scan() -> Scan {
    let group = DiscoveryKey("/sys/devices/synthetic-usb".into());
    let capabilities = Capabilities {
        events: Metadata::Known([0, 1, 17].into()),
        codes: BTreeMap::from([
            (
                1,
                Metadata::Known(crate::input::classification::typing_keys().collect()),
            ),
            (17, Metadata::Known([1].into())),
        ]),
        properties: Metadata::Known(Default::default()),
    };
    let classification =
        crate::input::classification::classify(&InputProperties::new(), &capabilities);
    Scan {
        groups: vec![GroupDescriptor {
            key: group.clone(),
            incarnation: Some(Incarnation {
                device: 1,
                inode: 1,
            }),
            usb: Metadata::Known(UsbIds {
                vendor: 0xfffe,
                product: 0x0001,
            }),
            serial: Metadata::Missing,
            identification: Identification::Unknown,
        }],
        endpoints: vec![EndpointDescriptor {
            key: DiscoveryKey("/sys/devices/synthetic-usb/input0/event0".into()),
            incarnation: Some(Incarnation {
                device: 1,
                inode: 2,
            }),
            group,
            group_confirmed: true,
            devnode: Metadata::Known("/dev/input/event0".into()),
            name: Metadata::Known("Synthetic keyboard".into()),
            physical_path: Metadata::Missing,
            unique_name: Metadata::Missing,
            identity: InputIdentity {
                bus: Metadata::Known(3),
                vendor: Metadata::Known(0xfffe),
                product: Metadata::Known(1),
                version: Metadata::Known(0),
            },
            capabilities,
            classification,
            input_properties: InputProperties::new(),
            placement: Placement::default(),
            stream_access: StreamAccess::NotChecked,
        }],
        complete: true,
        problems: vec![],
    }
}
pub(crate) fn failure() -> Problem {
    Problem {
        path: "/sys/class/input".into(),
        operation: "enumerate metadata".into(),
        kind: ProblemKind::Permission,
        os_code: Some(13),
    }
}

/// Independent advertised units with two input siblings each; equal labels never imply pairing.
pub(crate) fn devices() -> Scan {
    let template = scan();
    let mut result = Scan {
        groups: vec![],
        endpoints: vec![],
        complete: true,
        problems: vec![],
    };
    for unit in 0..2 {
        let mut group = template.groups[0].clone();
        group.key = DiscoveryKey(format!("/sys/devices/unit{unit}").into());
        group.serial = Metadata::Known(format!("unit-{unit}"));
        result.groups.push(group.clone());
        for sibling in 0..2 {
            let index = unit * 2 + sibling;
            let mut endpoint = template.endpoints[0].clone();
            endpoint.group = group.key.clone();
            endpoint.key =
                DiscoveryKey(format!("/sys/devices/unit{unit}/input{index}/event{index}").into());
            endpoint.devnode = Metadata::Known(format!("/dev/input/event{index}").into());
            endpoint.incarnation.as_mut().unwrap().inode = index + 2;
            result.endpoints.push(endpoint);
        }
    }
    result
}

/// Synthetic capability profiles, not captured GMMK/Defy interoperability evidence.
pub(crate) fn inventory() -> Scan {
    let profiles: [(&str, Option<Vec<u16>>); 13] = [
        (
            "GMMK Pro",
            Some(crate::input::classification::typing_keys().collect()),
        ),
        (
            "Dygma Defy",
            Some(crate::input::classification::typing_keys().collect()),
        ),
        (
            "AT Translated Set 2 keyboard",
            Some(crate::input::classification::typing_keys().collect()),
        ),
        ("Numeric keypad", Some((71..=83).collect())),
        ("Macro pad", Some(vec![183, 184])),
        ("Power Button", Some(vec![116])),
        ("Lid Switch", Some(vec![])),
        ("Video Bus", Some(vec![224, 225])),
        ("Audio controls", Some(vec![113, 114, 115])),
        ("Mouse", Some(vec![0x110, 0x111])),
        (
            "Virtual typing",
            Some(crate::input::classification::typing_keys().collect()),
        ),
        ("Virtual controls", Some(vec![113, 116])),
        ("Unreadable input", None),
    ];
    let template = scan();
    let mut result = Scan {
        groups: vec![],
        endpoints: vec![],
        complete: true,
        problems: vec![],
    };
    for (index, (name, keys)) in profiles.into_iter().enumerate() {
        let mut group = template.groups[0].clone();
        group.key = DiscoveryKey(format!("/sys/devices/fixture{index}").into());
        group.serial = Metadata::Known(format!("fixture-{index}"));
        let mut endpoint = template.endpoints[0].clone();
        endpoint.group = group.key.clone();
        endpoint.key = DiscoveryKey(group.key.0.join(format!("input{index}/event{index}")));
        endpoint.devnode = Metadata::Known(format!("/dev/input/event{index}").into());
        endpoint.name = Metadata::Known(name.into());
        endpoint.capabilities.codes.insert(
            1,
            keys.map(|k| Metadata::Known(k.into_iter().collect()))
                .unwrap_or_else(|| Metadata::Failed(failure())),
        );
        if index == 6 {
            endpoint.capabilities.events = Metadata::Known([0, 5].into());
        }
        endpoint.placement.origin = if [10, 11].contains(&index) {
            InputOrigin::Virtual
        } else {
            InputOrigin::Physical
        };
        if index == 2 {
            endpoint.identity.bus = Metadata::Known(0x11);
            endpoint.placement.location = InputLocation::Internal;
            endpoint.placement.reason = "i8042_on_portable_chassis";
        } else if index < 2 {
            endpoint.placement.location = InputLocation::External;
            endpoint.placement.reason = "removable_usb_ancestry";
        }
        if [2, 10, 11].contains(&index) {
            group.usb = Metadata::Missing;
            group.serial = Metadata::Missing;
        }
        endpoint.classification = crate::input::classification::classify(
            &endpoint.input_properties,
            &endpoint.capabilities,
        );
        result.groups.push(group);
        result.endpoints.push(endpoint.clone());
        if index < 2 {
            let mut media = endpoint;
            media.key.0 = media.key.0.with_file_name(format!("event{}", index + 20));
            media.devnode = Metadata::Known(format!("/dev/input/event{}", index + 20).into());
            media
                .capabilities
                .codes
                .insert(1, Metadata::Known([113, 114, 115].into()));
            media.classification = crate::input::classification::classify(
                &media.input_properties,
                &media.capabilities,
            );
            result.endpoints.push(media);
        }
    }
    result
}
