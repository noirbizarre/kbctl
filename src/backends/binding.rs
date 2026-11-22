use std::collections::BTreeMap;

use crate::{
    model::{
        capability::CapabilityScope,
        connection::{AttachmentToken, ConnectionState, OperationToken, TransportKind},
        identity::{AttachmentGeneration, BackendSessionId, ComponentId, EndpointId, KeyboardId},
        topology::Topology,
    },
    transport::LeaseSet,
};

use super::error::{BackendError, FailureKind, Result};

struct EndpointBinding {
    generation: AttachmentGeneration,
    kind: TransportKind,
    token: AttachmentToken,
}
struct Route {
    endpoint: EndpointId,
    token: OperationToken,
}

/// Immutable context captured before I/O, not reconstructed when an old event is
/// received. It is move-only so two sessions cannot share one session identity.
pub(crate) struct ConnectContext {
    id: BackendSessionId,
    keyboard: KeyboardId,
    primary: EndpointId,
    endpoints: BTreeMap<EndpointId, EndpointBinding>,
    routes: BTreeMap<ComponentId, Route>,
}

fn invalid(error: crate::model::ModelError) -> BackendError {
    BackendError::new(
        FailureKind::InvalidInput,
        "cannot establish a live backend binding",
    )
    .caused_by(error)
}

impl ConnectContext {
    pub(crate) fn new(
        topology: &Topology,
        primary: EndpointId,
        endpoints: Vec<EndpointId>,
        routes: Vec<(ComponentId, EndpointId)>,
    ) -> Result<Self> {
        let source = topology.endpoint(primary).map_err(invalid)?;
        let keyboard = topology
            .physical_device(source.physical())
            .map_err(invalid)?
            .keyboard();
        let mut bindings = BTreeMap::new();
        for endpoint in endpoints {
            let channel = topology.endpoint(endpoint).map_err(invalid)?;
            if topology
                .physical_device(channel.physical())
                .map_err(invalid)?
                .keyboard()
                != keyboard
            {
                return Err(BackendError::new(
                    FailureKind::InvalidTarget,
                    "selected endpoint belongs to another keyboard",
                ));
            }
            let token = topology.begin_attachment(endpoint).map_err(invalid)?;
            let ConnectionState::Ready(generation) = channel.state() else {
                unreachable!("validated attachment");
            };
            if bindings
                .insert(
                    endpoint,
                    EndpointBinding {
                        generation,
                        kind: channel.transport(),
                        token,
                    },
                )
                .is_some()
            {
                return Err(BackendError::new(
                    FailureKind::InvalidInput,
                    "duplicate selected endpoint",
                ));
            }
        }
        if !bindings.contains_key(&primary) {
            return Err(BackendError::new(
                FailureKind::InvalidInput,
                "primary capability source must be included in selected endpoints",
            ));
        }
        let mut selected_routes = BTreeMap::new();
        for (component, endpoint) in routes {
            if !bindings.contains_key(&endpoint) {
                return Err(BackendError::new(
                    FailureKind::InvalidTarget,
                    "route source is outside the selected endpoint set",
                ));
            }
            let token = topology
                .begin_operation(endpoint, component)
                .map_err(invalid)?;
            if selected_routes
                .insert(component, Route { endpoint, token })
                .is_some()
            {
                return Err(BackendError::new(
                    FailureKind::InvalidInput,
                    "duplicate component route",
                ));
            }
        }
        Ok(Self {
            id: BackendSessionId::new().map_err(invalid)?,
            keyboard,
            primary,
            endpoints: bindings,
            routes: selected_routes,
        })
    }
    pub(crate) fn id(&self) -> BackendSessionId {
        self.id
    }
    pub(super) fn primary(&self) -> EndpointId {
        self.primary
    }
    pub(crate) fn keyboard(&self) -> KeyboardId {
        self.keyboard
    }
    pub(super) fn component_source(&self, component: ComponentId) -> Result<EndpointId> {
        self.routes
            .get(&component)
            .map(|route| route.endpoint)
            .ok_or_else(|| {
                BackendError::new(
                    FailureKind::InvalidTarget,
                    "component has no authorized session route",
                )
            })
    }
    pub(super) fn source_for(&self, scope: CapabilityScope) -> Result<EndpointId> {
        match scope {
            CapabilityScope::Keyboard(keyboard) if keyboard == self.keyboard => Ok(self.primary),
            CapabilityScope::Component(component) => self.component_source(component),
            _ => Err(BackendError::new(
                FailureKind::InvalidTarget,
                "keyboard is outside this session",
            )),
        }
    }
    pub(super) fn admit(&self, leases: &LeaseSet) -> Result<()> {
        if leases.iter().count() != self.endpoints.len() {
            return Err(BackendError::new(
                FailureKind::InvalidInput,
                "lease set differs from selected endpoints",
            ));
        }
        for lease in leases.iter() {
            if !self
                .endpoints
                .get(&lease.endpoint())
                .is_some_and(|binding| {
                    binding.generation == lease.attachment() && binding.kind == lease.kind()
                })
            {
                return Err(BackendError::new(
                    FailureKind::InvalidInput,
                    "lease kind or attachment differs from the captured binding",
                ));
            }
        }
        Ok(())
    }
    pub(super) fn endpoints(&self) -> impl Iterator<Item = EndpointId> + '_ {
        self.endpoints.keys().copied()
    }
    pub(crate) fn component_routes(&self) -> impl Iterator<Item = (ComponentId, EndpointId)> + '_ {
        self.routes.iter().map(|(id, route)| (*id, route.endpoint))
    }
    pub(super) fn provenance(
        &self,
        reporter: &str,
        component: ComponentId,
    ) -> Result<crate::model::state::DeviceProvenance> {
        let endpoint = self.component_source(component)?;
        Ok(crate::model::state::DeviceProvenance {
            reporter: reporter.into(),
            session: self.id,
            attachment: self.endpoints[&endpoint].token,
            operation: self.routes[&component].token,
        })
    }
    pub(super) fn validates_replacement(&self, replacement: &Self) -> bool {
        replacement.id != self.id
            && replacement.endpoints.iter().all(|(id, binding)| {
                self.endpoints
                    .get(id)
                    .is_none_or(|old| old.generation != binding.generation)
            })
    }
    pub(super) fn validate_live(&self, topology: &Topology) -> Result<()> {
        for binding in self.endpoints.values() {
            topology.validate_attachment(binding.token).map_err(|e| {
                BackendError::new(FailureKind::StaleResult, "session attachment changed")
                    .caused_by(e)
            })?;
        }
        for route in self.routes.values() {
            topology.validate_completion(route.token).map_err(|e| {
                BackendError::new(
                    FailureKind::StaleResult,
                    "session component or membership changed",
                )
                .caused_by(e)
            })?;
        }
        Ok(())
    }
}
