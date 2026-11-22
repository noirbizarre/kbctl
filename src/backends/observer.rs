//! Read-only validation/liveness can outlive an I/O borrow, never its authority.

use super::{
    ConnectContext, ObservationDelivery,
    error::{BackendError, FailureKind, Phase, Result},
    session::SessionState,
};
use crate::model::{
    identity::{BackendSessionId, EndpointId},
    state::DeviceProvenance,
    time::IngressStamp,
    topology::Topology,
};
use std::{
    sync::{Arc, Mutex},
    time::Instant,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionLife {
    pub(crate) state: SessionState,
    pub(crate) stopped_at: Option<Instant>,
    pub(crate) failure: Option<FailureKind>,
    pub(crate) gap: Option<IngressStamp>,
}

#[derive(Clone)]
pub(crate) struct SessionObserver {
    pub(super) context: Arc<ConnectContext>,
    pub(super) life: Arc<Mutex<SessionLife>>,
    pub(super) backend: String,
}
impl SessionObserver {
    pub(crate) fn id(&self) -> BackendSessionId {
        self.context.id()
    }
    pub(crate) fn reporter(&self) -> &str {
        &self.backend
    }
    pub(crate) fn life(&self) -> SessionLife {
        *self.life.lock().expect("session life lock poisoned")
    }
    pub(crate) fn endpoints(&self) -> impl Iterator<Item = EndpointId> + '_ {
        self.context.endpoints()
    }
    pub(crate) fn sources(&self) -> Vec<DeviceProvenance> {
        self.context
            .component_routes()
            .map(|(component, _)| {
                self.context
                    .provenance(&self.backend, component)
                    .expect("captured route")
            })
            .collect()
    }
    pub(crate) fn validate_binding(&self, topology: &Topology) -> Result<()> {
        self.context
            .validate_live(topology)
            .map_err(|error| error.context(&self.backend, Phase::Acceptance, None, None))
    }
    pub(crate) fn validate(&self, topology: &Topology) -> Result<()> {
        if self.life().state != SessionState::Active {
            return Err(BackendError::new(
                FailureKind::Closed,
                "observation source session has ended",
            )
            .context(&self.backend, Phase::Acceptance, None, None));
        }
        self.validate_binding(topology)
    }
    pub(crate) fn accept<'a>(
        &self,
        topology: &Topology,
        delivery: &'a ObservationDelivery,
    ) -> Result<&'a crate::model::observation::DeviceObservation> {
        self.validate(topology)?;
        if delivery.session != self.id()
            || self.context.component_source(delivery.component)? != delivery.source
        {
            return Err(BackendError::new(
                FailureKind::StaleResult,
                "delivery does not belong to this captured source",
            )
            .context(
                &self.backend,
                Phase::Acceptance,
                None,
                Some(delivery.source),
            ));
        }
        Ok(&delivery.value)
    }
}
