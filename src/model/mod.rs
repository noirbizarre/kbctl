//! In-memory keyboard topology, independent of discovery and firmware protocols.
//!
//! Physical devices are host-discovered units, not USB interfaces. Only explicit
//! caller-directed assembly associates those units into a logical keyboard.
//! Mutations validate before publication; adapters receive immutable records.

// The next application layer will consume these private types. Keep the model
// compiled in the binary now without manufacturing CLI behavior to use it.
#![allow(dead_code)]

pub(crate) mod capability;
pub(crate) mod connection;
pub(crate) mod identity;
pub(crate) mod input;
pub(crate) mod observation;
pub(crate) mod state;
pub(crate) mod time;
pub(crate) mod topology;

use identity::{ComponentId, EndpointId, KeyboardId, PhysicalDeviceId};
use miette::Diagnostic;
use thiserror::Error;

pub(crate) type Result<T> = std::result::Result<T, ModelError>;

/// Invalid model input or work that no longer belongs to the live topology.
#[derive(Debug, Error, Diagnostic, PartialEq, Eq)]
pub(crate) enum ModelError {
    #[error("the process exhausted its runtime identifiers")]
    #[diagnostic(
        code(kbctl::model::id_exhausted),
        help("Restart kbctl before discovering more devices.")
    )]
    IdExhausted,
    #[error("{field} must not be empty or whitespace")]
    #[diagnostic(
        code(kbctl::model::empty_value),
        help("Supply a nonblank value, or represent missing metadata explicitly.")
    )]
    EmptyValue { field: &'static str },
    #[error("identity evidence repeats namespace `{namespace}` in the same scope")]
    #[diagnostic(
        code(kbctl::model::duplicate_claim),
        help("Reconcile the identity claims before building the record.")
    )]
    DuplicateClaim { namespace: String },
    #[error("keyboard {0} is not present")]
    #[diagnostic(
        code(kbctl::model::unknown_keyboard),
        help("Refresh the topology and use a live keyboard ID.")
    )]
    UnknownKeyboard(KeyboardId),
    #[error("physical device {0} is not present")]
    #[diagnostic(
        code(kbctl::model::unknown_physical_device),
        help("Refresh discovery and use a live physical-device ID.")
    )]
    UnknownPhysicalDevice(PhysicalDeviceId),
    #[error("endpoint {0} is not present")]
    #[diagnostic(
        code(kbctl::model::unknown_endpoint),
        help("Refresh discovery and use a live endpoint ID.")
    )]
    UnknownEndpoint(EndpointId),
    #[error("component {0} is not present")]
    #[diagnostic(
        code(kbctl::model::unknown_component),
        help("Refresh the topology and use a live component ID.")
    )]
    UnknownComponent(ComponentId),
    #[error("{kind} reference {index} is outside the physical-device specification")]
    #[diagnostic(
        code(kbctl::model::invalid_reference),
        help("Reference an entry in the same physical-device specification.")
    )]
    InvalidReference { kind: &'static str, index: usize },
    #[error("component {index} participates in a parent cycle")]
    #[diagnostic(
        code(kbctl::model::component_cycle),
        help("Remove self-parenting or cyclic parent links from the specification.")
    )]
    ComponentCycle { index: usize },
    #[error("USB interface number {number} is repeated within one physical device")]
    #[diagnostic(
        code(kbctl::model::duplicate_interface),
        help("Group channels under one interface record for each interface number.")
    )]
    DuplicateInterface { number: u8 },
    #[error("component {component} is not represented through physical device {physical}")]
    #[diagnostic(
        code(kbctl::model::cross_owner),
        help("Use a parent component from the same physical device.")
    )]
    CrossOwner {
        component: ComponentId,
        physical: PhysicalDeviceId,
    },
    #[error("endpoint {endpoint} cannot target component {component} in another keyboard")]
    #[diagnostic(
        code(kbctl::model::cross_keyboard),
        help("Establish keyboard membership before beginning an operation.")
    )]
    CrossKeyboard {
        endpoint: EndpointId,
        component: ComponentId,
    },
    #[error("endpoint {0} has no validated attachment")]
    #[diagnostic(
        code(kbctl::model::endpoint_not_ready),
        help("Resolve access problems and validate the endpoint before retrying.")
    )]
    EndpointNotReady(EndpointId),
    #[error("work for endpoint {endpoint} and component {component} is no longer current")]
    #[diagnostic(
        code(kbctl::model::stale_operation),
        help("Discard the result, reconcile the topology and start a new operation.")
    )]
    StaleOperation {
        endpoint: EndpointId,
        component: ComponentId,
    },
    #[error("capability operation {0:?} is repeated in one report")]
    #[diagnostic(
        code(kbctl::model::duplicate_capability),
        help("Supply one support entry per operation in the replacement report.")
    )]
    DuplicateCapability(capability::CapabilityOperation),
    #[error("{kind:?} version namespace `{namespace}` is repeated in one report")]
    #[diagnostic(
        code(kbctl::model::duplicate_capability_version),
        help("Reconcile version facts before publishing the capability report.")
    )]
    DuplicateCapabilityVersion {
        kind: capability::VersionKind,
        namespace: String,
    },
    #[error("endpoint {endpoint} belongs to a different keyboard from capability scope {scope:?}")]
    #[diagnostic(
        code(kbctl::model::capability_source_mismatch),
        help("Use a ready source endpoint belonging to the target keyboard.")
    )]
    CapabilitySourceMismatch {
        endpoint: EndpointId,
        scope: capability::CapabilityScope,
    },
    #[error("capability refresh {refresh} for {scope:?} is no longer current")]
    #[diagnostic(
        code(kbctl::model::stale_capability_refresh),
        help(
            "Discard the result and begin a new refresh after reconciling the target and source."
        )
    )]
    StaleCapabilityRefresh {
        refresh: identity::CapabilityRefreshId,
        scope: capability::CapabilityScope,
    },
    #[error("battery percentage {value} is outside 0..=100")]
    #[diagnostic(
        code(kbctl::model::invalid_percentage),
        help("Reject the malformed reading rather than clamping or reporting a fabricated value.")
    )]
    InvalidPercentage { value: u16 },
    #[error("attachment for endpoint {0} is no longer current")]
    #[diagnostic(
        code(kbctl::model::stale_attachment),
        help("Discard old work and establish a new binding after revalidating the endpoint.")
    )]
    StaleAttachment(EndpointId),
    #[error("endpoint {index} has HID collection metadata on a non-HID transport")]
    #[diagnostic(
        code(kbctl::model::unexpected_hid_metadata),
        help("Attach usage facts only to the HID collection that actually reported them.")
    )]
    UnexpectedHidMetadata { index: usize },
    #[error("invalid normalized status value: {0}")]
    #[diagnostic(
        code(kbctl::model::invalid_status),
        help("Supply a validated, backend-independent value; leave unreported fields absent.")
    )]
    InvalidStatus(&'static str),
}

#[cfg(test)]
mod tests;
