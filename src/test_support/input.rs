//! Synthetic input metadata, never a claim about attached hardware.
use crate::model::{identity::Identification, input::*, topology::UsbIds};
use std::collections::BTreeMap;

pub(crate) fn scan() -> Scan {
    let group = DiscoveryKey("/sys/devices/synthetic-usb".into());
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
            capabilities: Capabilities {
                events: Metadata::Known([0, 1, 17].into()),
                codes: BTreeMap::from([
                    (1, Metadata::Known((16..=25).collect())),
                    (17, Metadata::Known([1].into())),
                ]),
                properties: Metadata::Known(Default::default()),
            },
            keyboard: KeyboardClassification::Known {
                keyboard: true,
                evidence: ClassificationEvidence::KeyBitmap,
            },
            classification_property: Metadata::Missing,
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
