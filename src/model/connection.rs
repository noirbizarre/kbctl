//! Attachment freshness only; opening hardware and scheduling retries are adapter policy.

use super::{
    Result,
    identity::{
        AttachmentGeneration, ComponentId, EndpointId, KeyboardId, MembershipGeneration,
        PhysicalDeviceId, nonblank,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum TransportKind {
    Hid,
    Serial,
}

/// Transient discovery locator, deliberately excluded from hardware evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EndpointLocator(String);

impl EndpointLocator {
    pub(crate) fn new(value: impl Into<String>) -> Result<Self> {
        nonblank(value.into(), "endpoint locator").map(Self)
    }
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnavailableReason {
    PermissionDenied,
    Busy,
    Disconnected,
    Suspended,
    ProtocolFailure,
}

/// Ready denotes a validated channel, not support for any particular protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ConnectionState {
    #[default]
    Unvalidated,
    Ready(AttachmentGeneration),
    Unavailable(UnavailableReason),
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Connection {
    state: ConnectionState,
}

impl Connection {
    pub(super) fn new() -> Self {
        Self {
            state: ConnectionState::Unvalidated,
        }
    }
    pub(crate) fn state(&self) -> ConnectionState {
        self.state
    }

    pub(super) fn revalidate(&mut self) -> Result<AttachmentGeneration> {
        let generation = AttachmentGeneration::allocate()?;
        self.state = ConnectionState::Ready(generation);
        Ok(generation)
    }

    pub(super) fn invalidate(&mut self, reason: UnavailableReason) {
        // Dropping Ready also drops the only generation accepted for new work.
        self.state = ConnectionState::Unavailable(reason);
    }
}

/// Validates instance/attachment freshness, not backend routing authorization.
/// The application must serialize checking this token with applying its result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OperationToken {
    pub(super) endpoint: EndpointId,
    pub(super) generation: AttachmentGeneration,
    pub(super) component: ComponentId,
    pub(super) keyboard: KeyboardId,
    pub(super) endpoint_membership: MembershipGeneration,
    pub(super) target_membership: MembershipGeneration,
}

/// Captured before connecting, including for keyboards with no known components.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AttachmentToken {
    pub(super) endpoint: EndpointId,
    pub(super) generation: AttachmentGeneration,
    pub(super) physical: PhysicalDeviceId,
    pub(super) keyboard: KeyboardId,
    pub(super) membership: MembershipGeneration,
}
