//! Reviewed identification facts only; these references enable no hardware driver.

use super::*;
use crate::model::topology::Side;

const QMK: &str = "https://github.com/qmk/qmk_firmware/blob/80973aa0b5413e9a5bbbf9293507f39e870f9aa3/keyboards/gmmk/pro/rev1/ansi/keyboard.json";
const DY_WIRED: &str = "https://github.com/Dygmalab/Bazecor/blob/f49f25b11ce21ec22cd8fd6ad44410d749793c07/src/api/hardware-dygma-defy-wired/index.ts";
const DY_WIRELESS: &str = "https://github.com/Dygmalab/Bazecor/blob/f49f25b11ce21ec22cd8fd6ad44410d749793c07/src/api/hardware-dygma-defy-wireless/index.ts";
const DY_SERIAL: &str =
    "https://github.com/Dygmalab/Bazecor/blob/f49f25b11ce21ec22cd8fd6ad44410d749793c07/README.md";
const NAYA_IDS: &str = "https://github.com/Qonfused/nayactl/blob/8049c9f94d4df9f9c432574f1ae68b10578c38ef/src/nayactl/constants.py";
const NAYA_DISCOVERY: &str = "https://github.com/Qonfused/nayactl/blob/8049c9f94d4df9f9c432574f1ae68b10578c38ef/src/nayactl/discovery.py";

fn source(id: &str, class: SourceClass, reference: &str, scope: &str) -> SourceRecord {
    SourceRecord {
        id: id.into(),
        class,
        reference: reference.into(),
        scope: scope.into(),
    }
}
fn template(key: &str, kind: ComponentKind, parent: Option<&str>) -> ComponentTemplate {
    ComponentTemplate {
        key: key.into(),
        kind,
        parent: parent.map(str::to_owned),
    }
}
fn serial_reference(backend: &str, configuration: &str, source: &str) -> ProtocolReference {
    ProtocolReference {
        id: format!("{backend}.management"),
        backend: backend.into(),
        configuration: configuration.into(),
        status: MappingStatus::ResearchOnly,
        primary_role: "management".into(),
        roles: vec![EndpointRole {
            key: "management".into(),
            selector: EndpointSelector {
                transport: TransportKind::Serial,
                interface_number: None,
                hid: HidMetadata::default(),
            },
        }],
        sources: vec![source.into()],
    }
}

pub(crate) fn registry() -> Result<Registry> {
    let sources = vec![
        source(
            "qmk.gmmk-pro.rev1-ansi",
            SourceClass::Upstream,
            QMK,
            "QMK target USB identifier; does not establish installed firmware, stock protocol, management interface or other revisions",
        ),
        source(
            "dygma.defy.wired",
            SourceClass::Vendor,
            DY_WIRED,
            "Vendor runtime and bootloader IDs for wired Defy; no verified KBCTL command/firmware range",
        ),
        source(
            "dygma.defy.wireless",
            SourceClass::Vendor,
            DY_WIRELESS,
            "Vendor runtime and bootloader IDs for wireless Defy; not a Bluetooth management claim",
        ),
        source(
            "dygma.serial",
            SourceClass::Vendor,
            DY_SERIAL,
            "Bazecor communication requires USB serial; interface selection and safe KBCTL queries remain unverified",
        ),
        source(
            "naya.usb",
            SourceClass::Community,
            NAYA_IDS,
            "Independent implementation's left/right/dongle IDs, not manufacturer or KBCTL hardware validation",
        ),
        source(
            "naya.discovery",
            SourceClass::Community,
            NAYA_DISCOVERY,
            "Serial ports filtered by VID/PID; serial is optional and establishes neither uniqueness nor pairing",
        ),
    ];
    let mut definitions = vec![DeviceDefinition {
        id: "glorious.gmmk-pro.rev1-ansi".into(), model: "Glorious GMMK Pro".into(), family: "glorious.gmmk-pro".into(), variant: "rev1 ANSI identifier from QMK".into(), mode: DeviceMode::Runtime,
        selector: PhysicalSelector { usb: UsbIds { vendor: 0x320f, product: 0x5044 }, serial: None },
        components: vec![template("body", ComponentKind::Body, None)], protocols: vec![], sources: vec!["qmk.gmmk-pro.rev1-ansi".into()],
        limitations: "Identifier matches an upstream target, not installed QMK/VIA/stock firmware; no management interface/usage selector is verified".into(),
    }];
    for (variant, runtime, bootloader, source) in [
        ("wired", 0x0010, 0x0011, "dygma.defy.wired"),
        ("wireless", 0x0012, 0x0013, "dygma.defy.wireless"),
    ] {
        definitions.push(DeviceDefinition {
            id: format!("dygma.defy.{variant}"), model: format!("Dygma Defy {variant}"), family: "dygma.defy".into(), variant: variant.into(), mode: DeviceMode::Runtime,
            selector: PhysicalSelector { usb: UsbIds { vendor: 0x35ef, product: runtime }, serial: None },
            components: vec![template("controller", ComponentKind::Controller, None), template("left", ComponentKind::Half(Some(Side::Left)), Some("controller")), template("right", ComponentKind::Half(Some(Side::Right)), Some("controller"))],
            protocols: vec![serial_reference("dygma.serial", &format!("defy-{variant}"), "dygma.serial")], sources: vec![source.into()],
            limitations: "Controller/halves are expected topology, not observed presence; serial protocol reference is research-only with no capability or safe-query claim".into(),
        });
        definitions.push(DeviceDefinition {
            id: format!("dygma.defy.{variant}.bootloader"),
            model: format!("Dygma Defy {variant} bootloader"),
            family: "dygma.defy".into(),
            variant: variant.into(),
            mode: DeviceMode::Bootloader,
            selector: PhysicalSelector {
                usb: UsbIds {
                    vendor: 0x35ef,
                    product: bootloader,
                },
                serial: None,
            },
            components: vec![],
            protocols: vec![],
            sources: vec![source.into()],
            limitations:
                "Bootloader identification only; never an ordinary runtime management candidate"
                    .into(),
        });
    }
    for (role, product, kind) in [
        ("left", 0x0064, ComponentKind::Half(Some(Side::Left))),
        ("right", 0x00c8, ComponentKind::Half(Some(Side::Right))),
        ("dongle", 0x012c, ComponentKind::Dongle),
    ] {
        definitions.push(DeviceDefinition {
            id: format!("naya.create.{role}"), model: format!("Naya Create {role}"), family: "naya.create".into(), variant: role.into(), mode: DeviceMode::Runtime,
            selector: PhysicalSelector { usb: UsbIds { vendor: 0x37d1, product }, serial: None }, components: vec![template(role, kind, None)],
            protocols: vec![serial_reference("naya.cdc", &format!("create-{role}"), "naya.discovery")], sources: vec!["naya.usb".into()],
            limitations: "Community source, no hardware validation or firmware range; side labels, missing/duplicate serials and family names never establish split pairing".into(),
        });
    }
    Registry::new(definitions, sources, 4096)
}
