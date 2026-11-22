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
