use std::{error::Error as StdError, fmt, sync::Arc};

use miette::Diagnostic;
use thiserror::Error;

use crate::{
    model::{capability::CapabilityOperation, identity::EndpointId},
    transport::{TransportError, TransportFailure},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureKind {
    Unsupported,
    SupportUnknown,
    InvalidInput,
    InvalidTarget,
    Closed,
    Busy,
    AccessDenied,
    ExclusiveAccess,
    DeadlineExceeded,
    Cancelled,
    Disconnected,
    Protocol,
    ResourceLimit,
    ResyncRequired,
    StaleResult,
}

impl FailureKind {
    fn code(self) -> &'static str {
        match self {
            Self::Unsupported => "kbctl::backend::unsupported",
            Self::SupportUnknown => "kbctl::backend::support_unknown",
            Self::InvalidInput => "kbctl::backend::invalid_input",
            Self::InvalidTarget => "kbctl::backend::invalid_target",
            Self::Closed => "kbctl::backend::closed",
            Self::Busy => "kbctl::backend::busy",
            Self::AccessDenied => "kbctl::backend::access_denied",
            Self::ExclusiveAccess => "kbctl::backend::exclusive_access",
            Self::DeadlineExceeded => "kbctl::backend::deadline",
            Self::Cancelled => "kbctl::backend::cancelled",
            Self::Disconnected => "kbctl::backend::disconnected",
            Self::Protocol => "kbctl::backend::protocol",
            Self::ResourceLimit => "kbctl::backend::resource_limit",
            Self::ResyncRequired => "kbctl::backend::resync_required",
            Self::StaleResult => "kbctl::backend::stale_result",
        }
    }
    fn help(self) -> &'static str {
        match self {
            Self::Unsupported => {
                "Select an operation supported by this firmware and target; do not retry unchanged."
            }
            Self::SupportUnknown => "Refresh capability evidence before executing this operation.",
            Self::InvalidInput | Self::InvalidTarget => {
                "Reconcile discovery, routes and selected endpoints before connecting or dispatching."
            }
            Self::Busy | Self::ExclusiveAccess => {
                "Release competing access and reopen with the protocol's required grant."
            }
            Self::AccessDenied => {
                "Resolve device permissions or seat access, then reopen and reconnect."
            }
            Self::Closed | Self::Disconnected | Self::Cancelled | Self::DeadlineExceeded => {
                "Reconnect with fresh leases and attachment context before retrying admitted work."
            }
            Self::Protocol | Self::ResourceLimit => {
                "Keep the original diagnostic context and verify the protocol/firmware and configured bounds before reconnecting."
            }
            Self::ResyncRequired => {
                "Recover current information with supported reads and reconnect to restore event delivery."
            }
            Self::StaleResult => {
                "Discard this obsolete delivery and establish a current session binding."
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    Matching,
    Connect,
    Capabilities,
    Read,
    Events,
    Shutdown,
    Acceptance,
}

/// Shared causes let both the request caller and terminal event receiver retain
/// the same original diagnostic chain without flattening it into a string.
#[derive(Debug, Clone)]
struct SharedCause(Arc<dyn StdError + Send + Sync>);
impl fmt::Display for SharedCause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl StdError for SharedCause {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(self.0.as_ref())
    }
}

#[derive(Debug, Clone, Error)]
#[error(
    "{kind:?} during {phase:?} for backend {backend:?}, endpoint {endpoint:?}, operation {operation:?}: {detail}"
)]
pub(crate) struct BackendError {
    kind: FailureKind,
    phase: Phase,
    backend: Option<String>,
    endpoint: Option<EndpointId>,
    operation: Option<CapabilityOperation>,
    detail: String,
    #[source]
    cause: Option<SharedCause>,
}

impl BackendError {
    pub(crate) fn new(kind: FailureKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            phase: Phase::Connect,
            backend: None,
            endpoint: None,
            operation: None,
            detail: detail.into(),
            cause: None,
        }
    }
    pub(crate) fn caused_by(mut self, error: impl StdError + Send + Sync + 'static) -> Self {
        self.cause = Some(SharedCause(Arc::new(error)));
        self
    }
    pub(crate) fn context(
        mut self,
        backend: &str,
        phase: Phase,
        operation: Option<CapabilityOperation>,
        endpoint: Option<EndpointId>,
    ) -> Self {
        self.backend = Some(backend.to_owned());
        self.phase = phase;
        self.operation = operation;
        if self.endpoint.is_none() {
            self.endpoint = endpoint;
        }
        self
    }
    pub(crate) fn kind(&self) -> FailureKind {
        self.kind
    }
    pub(crate) fn endpoint(&self) -> Option<EndpointId> {
        self.endpoint
    }
    pub(crate) fn operation(&self) -> Option<CapabilityOperation> {
        self.operation
    }
    pub(crate) fn backend(&self) -> Option<&str> {
        self.backend.as_deref()
    }
}

impl Diagnostic for BackendError {
    fn code<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        Some(Box::new(self.kind.code()))
    }
    fn help<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        Some(Box::new(self.kind.help()))
    }
}

impl From<TransportError> for BackendError {
    fn from(error: TransportError) -> Self {
        let (kind, endpoint) = match &error {
            TransportError::Io { endpoint, kind, .. } => (
                match kind {
                    TransportFailure::Disconnected => FailureKind::Disconnected,
                    TransportFailure::AccessDenied => FailureKind::AccessDenied,
                    TransportFailure::Busy => FailureKind::Busy,
                    TransportFailure::Other => FailureKind::Protocol,
                },
                Some(*endpoint),
            ),
            TransportError::ExclusiveAccess(endpoint) => {
                (FailureKind::ExclusiveAccess, Some(*endpoint))
            }
            TransportError::ResourceLimit { endpoint, .. } => {
                (FailureKind::ResourceLimit, Some(*endpoint))
            }
            _ => (FailureKind::InvalidInput, None),
        };
        let mut failure = Self::new(kind, "injected transport failed").caused_by(error);
        failure.endpoint = endpoint;
        failure
    }
}

pub(crate) type Result<T> = std::result::Result<T, BackendError>;
