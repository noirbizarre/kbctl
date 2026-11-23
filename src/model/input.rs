//! Host input metadata is independent of programmable-keyboard topology and authority.

use super::{
    identity::{Identification, InputDeviceId, InputEndpointId, PhysicalDeviceId},
    topology::UsbIds,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Metadata<T> {
    Known(T),
    Missing,
    Failed(Problem),
}
impl<T> Metadata<T> {
    pub(crate) fn known(&self) -> Option<&T> {
        match self {
            Self::Known(value) => Some(value),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProblemKind {
    Permission,
    Io,
    Malformed,
    Race,
    Monitor,
    Unsupported,
}

/// Stable snapshot data; the diagnostic edge can additionally retain an OS error source.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, miette::Diagnostic)]
#[error("input discovery could not {} at {} ({:?}, OS code {:?})", .operation, .path.display(), .kind, .os_code)]
#[diagnostic(
    code(kbctl::input::discovery),
    help(
        "Discovery uses metadata only. Check the named sysfs/udev path, mount or container visibility, and path-specific access policy; retry after correcting access. Event-stream permissions have not been checked."
    )
)]
pub(crate) struct Problem {
    pub(crate) path: PathBuf,
    pub(crate) operation: String,
    pub(crate) kind: ProblemKind,
    pub(crate) os_code: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct DiscoveryKey(pub(crate) PathBuf);

/// Live source evidence only. Neither this fingerprint nor a syspath is persistent identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Incarnation {
    pub(crate) device: u64,
    pub(crate) inode: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InputIdentity {
    pub(crate) bus: Metadata<u16>,
    pub(crate) vendor: Metadata<u16>,
    pub(crate) product: Metadata<u16>,
    pub(crate) version: Metadata<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Capabilities {
    pub(crate) events: Metadata<BTreeSet<u16>>,
    pub(crate) codes: BTreeMap<u16, Metadata<BTreeSet<u16>>>,
    pub(crate) properties: Metadata<BTreeSet<u16>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputClass {
    Keyboard,
    Partial,
    Other,
    Unknown,
}
impl InputClass {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Keyboard => "keyboard",
            Self::Partial => "partial_keyboard",
            Self::Other => "other",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClassificationReason {
    TypingProfile,
    TypingSubset,
    NonKeyboardRole,
    NoTypingKeys,
    MissingCapabilities,
    ConflictingMetadata,
}
impl ClassificationReason {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::TypingProfile => "typing_profile",
            Self::TypingSubset => "typing_subset",
            Self::NonKeyboardRole => "non_keyboard_role",
            Self::NoTypingKeys => "no_typing_keys",
            Self::MissingCapabilities => "missing_capabilities",
            Self::ConflictingMetadata => "conflicting_metadata",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct TypingCoverage {
    pub(crate) letters: usize,
    pub(crate) digits: usize,
    pub(crate) editing: usize,
    pub(crate) modifiers: usize,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InputClassification {
    pub(crate) kind: InputClass,
    pub(crate) reason: ClassificationReason,
    pub(crate) coverage: Option<TypingCoverage>,
}
pub(crate) type InputProperties = BTreeMap<&'static str, Metadata<String>>;
pub(crate) const INPUT_PROPERTIES: &[&str] = &[
    "ID_INPUT_KEYBOARD",
    "ID_INPUT_KEY",
    "ID_INPUT_MOUSE",
    "ID_INPUT_TOUCHPAD",
    "ID_INPUT_TOUCHSCREEN",
    "ID_INPUT_TABLET",
    "ID_INPUT_JOYSTICK",
    "ID_INPUT_SWITCH",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputOrigin {
    Physical,
    Virtual,
    Unknown,
}
impl InputOrigin {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Physical => "physical",
            Self::Virtual => "virtual",
            Self::Unknown => "unknown",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputLocation {
    Internal,
    External,
    Unknown,
}
impl InputLocation {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Internal => "internal",
            Self::External => "external",
            Self::Unknown => "unknown",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlacementFact {
    pub(crate) path: PathBuf,
    pub(crate) value: Metadata<String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Placement {
    pub(crate) origin: InputOrigin,
    pub(crate) location: InputLocation,
    pub(crate) reason: &'static str,
    pub(crate) facts: Vec<PlacementFact>,
}
impl Default for Placement {
    fn default() -> Self {
        Self {
            origin: InputOrigin::Unknown,
            location: InputLocation::Unknown,
            reason: "insufficient_placement_evidence",
            facts: vec![],
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamAccess {
    NotChecked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GroupDescriptor {
    pub(crate) key: DiscoveryKey,
    pub(crate) incarnation: Option<Incarnation>,
    pub(crate) usb: Metadata<UsbIds>,
    pub(crate) serial: Metadata<String>,
    pub(crate) identification: Identification,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EndpointDescriptor {
    pub(crate) key: DiscoveryKey,
    pub(crate) incarnation: Option<Incarnation>,
    pub(crate) group: DiscoveryKey,
    pub(crate) group_confirmed: bool,
    pub(crate) devnode: Metadata<PathBuf>,
    pub(crate) name: Metadata<String>,
    pub(crate) physical_path: Metadata<String>,
    pub(crate) unique_name: Metadata<String>,
    pub(crate) identity: InputIdentity,
    pub(crate) capabilities: Capabilities,
    pub(crate) classification: InputClassification,
    pub(crate) input_properties: InputProperties,
    pub(crate) placement: Placement,
    pub(crate) stream_access: StreamAccess,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InputDevice {
    pub(crate) id: InputDeviceId,
    pub(crate) descriptor: GroupDescriptor,
    pub(crate) physical: Option<PhysicalDeviceId>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InputEndpoint {
    pub(crate) id: InputEndpointId,
    pub(crate) device: InputDeviceId,
    pub(crate) descriptor: EndpointDescriptor,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InventoryStatus {
    NotScanned,
    Current,
    Degraded(Vec<Problem>),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MonitorStatus {
    NotStarted,
    Watching,
    Degraded(Problem),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiscoveryStatus {
    pub(crate) inventory: InventoryStatus,
    pub(crate) monitor: MonitorStatus,
}
impl Default for DiscoveryStatus {
    fn default() -> Self {
        Self {
            inventory: InventoryStatus::NotScanned,
            monitor: MonitorStatus::NotStarted,
        }
    }
}

/// Membership completeness is separate from the readability of each metadata field.
#[derive(Debug, Clone)]
pub(crate) struct Scan {
    pub(crate) groups: Vec<GroupDescriptor>,
    pub(crate) endpoints: Vec<EndpointDescriptor>,
    pub(crate) complete: bool,
    pub(crate) problems: Vec<Problem>,
}
