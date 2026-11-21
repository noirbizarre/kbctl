//! Injected, already-opened channels; OS opens and process locking are host work.

// Compiled now for contract tests; real transport adapters arrive separately.
#![allow(dead_code)]

use std::{
    collections::BTreeSet,
    io,
    task::{Context, Poll},
};

use miette::Diagnostic;
use thiserror::Error;

use crate::model::{
    connection::TransportKind,
    identity::{AttachmentGeneration, EndpointId},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransportFailure {
    Disconnected,
    AccessDenied,
    Busy,
    Other,
}

#[derive(Debug, Error, Diagnostic)]
pub(crate) enum TransportError {
    #[error("invalid transport lease: {0}")]
    #[diagnostic(
        code(kbctl::transport::invalid_lease),
        help("Reconcile discovery and provide one validated lease per selected endpoint.")
    )]
    InvalidLease(&'static str),
    #[error("endpoint {0} was supplied more than once")]
    #[diagnostic(
        code(kbctl::transport::duplicate_endpoint),
        help("Transfer each endpoint to exactly one lease in this connection.")
    )]
    DuplicateEndpoint(EndpointId),
    #[error("endpoint {0} lacks the required transport-exclusive access grant")]
    #[diagnostic(
        code(kbctl::transport::exclusive_access),
        help("Release competing access and reopen with the protocol's required exclusivity.")
    )]
    ExclusiveAccess(EndpointId),
    #[error("transfer on endpoint {endpoint} exceeds its {limit}-byte bound")]
    #[diagnostic(
        code(kbctl::transport::resource_limit),
        help("Use the verified protocol's bounded transfer sizes; reject oversized replies.")
    )]
    ResourceLimit { endpoint: EndpointId, limit: usize },
    #[error("the requested I/O operation is incompatible with endpoint {0}")]
    #[diagnostic(
        code(kbctl::transport::wrong_kind),
        help("Use serial byte operations or HID report/feature operations as appropriate.")
    )]
    WrongKind(EndpointId),
    #[error("{kind:?} transport failure on endpoint {endpoint}")]
    #[diagnostic(
        code(kbctl::transport::io),
        help("Resolve access or connection problems and reopen the endpoint before retrying.")
    )]
    Io {
        endpoint: EndpointId,
        kind: TransportFailure,
        #[source]
        source: io::Error,
    },
}

pub(crate) type Result<T> = std::result::Result<T, TransportError>;

/// Distinct byte-stream/report semantics prevent accidentally treating a HID
/// feature transaction as a serial read or losing its report identifier.
pub(crate) enum IoRequest<'a> {
    SerialRead(&'a mut [u8]),
    SerialWrite(&'a [u8]),
    HidInput(&'a mut [u8]),
    HidOutput { report: u8, data: &'a [u8] },
    HidGetFeature { report: u8, data: &'a mut [u8] },
    HidSetFeature { report: u8, data: &'a [u8] },
}

impl IoRequest<'_> {
    fn kind(&self) -> TransportKind {
        match self {
            Self::SerialRead(_) | Self::SerialWrite(_) => TransportKind::Serial,
            _ => TransportKind::Hid,
        }
    }
    fn capacity(&self) -> usize {
        match self {
            Self::SerialRead(data) | Self::HidInput(data) | Self::HidGetFeature { data, .. } => {
                data.len()
            }
            Self::SerialWrite(data)
            | Self::HidOutput { data, .. }
            | Self::HidSetFeature { data, .. } => data.len(),
        }
    }
}

/// Pending operations must register their waker, never block. Implementations
/// own OS handles and must release them locally without awaiting device replies.
pub(crate) trait TransportIo: Send {
    fn kind(&self) -> TransportKind;
    fn poll_io(&mut self, cx: &mut Context<'_>, request: IoRequest<'_>) -> Poll<Result<usize>>;
    fn close(&mut self);
}

/// Asserted by the opener; neither grant proves exclusion of unrelated processes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AccessGrant {
    Cooperative,
    TransportExclusive,
}

pub(crate) struct TransportLease {
    endpoint: EndpointId,
    attachment: AttachmentGeneration,
    grant: AccessGrant,
    max_transfer: usize,
    io: Option<Box<dyn TransportIo>>,
}

impl TransportLease {
    pub(crate) fn new(
        endpoint: EndpointId,
        attachment: AttachmentGeneration,
        grant: AccessGrant,
        io: Box<dyn TransportIo>,
        max_transfer: usize,
    ) -> Result<Self> {
        let lease = Self {
            endpoint,
            attachment,
            grant,
            max_transfer,
            io: Some(io),
        };
        if max_transfer == 0 {
            return Err(TransportError::InvalidLease(
                "transfer bounds must be nonzero",
            ));
        }
        Ok(lease)
    }
    pub(crate) fn endpoint(&self) -> EndpointId {
        self.endpoint
    }
    pub(crate) fn attachment(&self) -> AttachmentGeneration {
        self.attachment
    }
    pub(crate) fn kind(&self) -> TransportKind {
        self.io.as_ref().expect("live lease").kind()
    }
    pub(crate) fn poll_io(
        &mut self,
        cx: &mut Context<'_>,
        request: IoRequest<'_>,
    ) -> Poll<Result<usize>> {
        if request.kind() != self.kind() {
            return Poll::Ready(Err(TransportError::WrongKind(self.endpoint)));
        }
        let capacity = request.capacity();
        if capacity > self.max_transfer {
            return Poll::Ready(Err(TransportError::ResourceLimit {
                endpoint: self.endpoint,
                limit: self.max_transfer,
            }));
        }
        match self.io.as_mut().expect("live lease").poll_io(cx, request) {
            Poll::Ready(Ok(size)) if size > capacity => {
                Poll::Ready(Err(TransportError::ResourceLimit {
                    endpoint: self.endpoint,
                    limit: capacity,
                }))
            }
            result => result,
        }
    }
}

impl Drop for TransportLease {
    fn drop(&mut self) {
        if let Some(mut io) = self.io.take() {
            io.close();
        }
    }
}

/// The application supplies its actual ownership guard here. This trait only
/// retains its lifetime; it does not acquire or prove an OS/process lock.
pub(crate) trait OwnershipReservation: Send {}

pub(crate) struct LeaseSet {
    leases: Vec<TransportLease>,
    _reservation: Box<dyn OwnershipReservation>,
}

impl LeaseSet {
    pub(crate) fn new(
        leases: Vec<TransportLease>,
        reservation: Box<dyn OwnershipReservation>,
    ) -> Result<Self> {
        let set = Self {
            leases,
            _reservation: reservation,
        };
        if set.leases.is_empty() {
            return Err(TransportError::InvalidLease(
                "a session needs at least one endpoint",
            ));
        }
        let mut seen = BTreeSet::new();
        for lease in &set.leases {
            if !seen.insert(lease.endpoint) {
                return Err(TransportError::DuplicateEndpoint(lease.endpoint));
            }
        }
        Ok(set)
    }
    pub(crate) fn iter(&self) -> impl Iterator<Item = &TransportLease> {
        self.leases.iter()
    }
    pub(crate) fn channel(&mut self, endpoint: EndpointId) -> Result<&mut TransportLease> {
        self.leases
            .iter_mut()
            .find(|lease| lease.endpoint == endpoint)
            .ok_or(TransportError::InvalidLease(
                "endpoint is outside the admitted lease set",
            ))
    }
    pub(crate) fn require_exclusive(&self) -> Result<()> {
        for lease in &self.leases {
            if lease.grant != AccessGrant::TransportExclusive {
                return Err(TransportError::ExclusiveAccess(lease.endpoint));
            }
        }
        Ok(())
    }
}

impl Drop for LeaseSet {
    fn drop(&mut self) {
        // Close/drop every I/O owner before the process reservation can release.
        self.leases.clear();
    }
}

#[cfg(test)]
mod tests;
