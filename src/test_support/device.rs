//! Topology builders, not a parallel device or identity model.
use crate::{
    model::{
        identity::{ComponentId, EndpointId, InterfaceId, KeyboardId, PhysicalDeviceId},
        topology::{PhysicalDeviceSpec, Topology},
    },
    test_support::transport::ScriptError,
};
use std::collections::BTreeMap;

pub(crate) struct DevicePart {
    pub(crate) alias: String,
    pub(crate) group: String,
    pub(crate) spec: PhysicalDeviceSpec,
    pub(crate) interfaces: Vec<String>,
    pub(crate) endpoints: Vec<String>,
    pub(crate) components: Vec<String>,
}
#[derive(Default)]
pub(crate) struct DeviceBuilder {
    parts: Vec<DevicePart>,
    routes: Vec<(String, String)>,
}
pub(crate) struct DeviceFixture {
    pub(crate) topology: Topology,
    pub(crate) physical: BTreeMap<String, PhysicalDeviceId>,
    pub(crate) groups: BTreeMap<String, KeyboardId>,
    pub(crate) interfaces: BTreeMap<String, InterfaceId>,
    pub(crate) endpoints: BTreeMap<String, EndpointId>,
    pub(crate) components: BTreeMap<String, ComponentId>,
    pub(crate) routes: Vec<(ComponentId, EndpointId)>,
}
fn insert<T>(map: &mut BTreeMap<String, T>, alias: String, value: T) -> Result<(), ScriptError> {
    if alias.trim().is_empty() || map.insert(alias, value).is_some() {
        return Err(ScriptError::new(
            "device builder",
            "blank or duplicate alias",
        ));
    }
    Ok(())
}
impl DeviceBuilder {
    pub(crate) fn new() -> Self {
        Self::default()
    }
    pub(crate) fn physical(mut self, part: DevicePart) -> Self {
        self.parts.push(part);
        self
    }
    pub(crate) fn route(mut self, component: &str, endpoint: &str) -> Self {
        self.routes.push((component.into(), endpoint.into()));
        self
    }
    pub(crate) fn build(self) -> Result<DeviceFixture, ScriptError> {
        let mut fixture = DeviceFixture {
            topology: Topology::new(),
            physical: BTreeMap::new(),
            groups: BTreeMap::new(),
            interfaces: BTreeMap::new(),
            endpoints: BTreeMap::new(),
            components: BTreeMap::new(),
            routes: vec![],
        };
        for part in self.parts {
            if part.group.trim().is_empty()
                || part.interfaces.len() != part.spec.interfaces.len()
                || part.endpoints.len() != part.spec.endpoints.len()
                || part.components.len() != part.spec.components.len()
            {
                return Err(ScriptError::new(
                    &part.alias,
                    "group and alias counts must match model specifications",
                ));
            }
            let physical = fixture
                .topology
                .add_physical_device(fixture.groups.get(&part.group).copied(), part.spec)
                .map_err(|error| ScriptError::new(&part.alias, error.to_string()))?;
            insert(&mut fixture.physical, part.alias, physical)?;
            fixture.groups.insert(
                part.group,
                fixture
                    .topology
                    .physical_device(physical)
                    .unwrap()
                    .keyboard(),
            );
            for (alias, value) in part
                .interfaces
                .into_iter()
                .zip(fixture.topology.interfaces(physical))
            {
                insert(&mut fixture.interfaces, alias, value.id())?;
            }
            for (alias, value) in part
                .endpoints
                .into_iter()
                .zip(fixture.topology.endpoints(physical))
            {
                insert(&mut fixture.endpoints, alias, value.id())?;
            }
            for (alias, value) in part
                .components
                .into_iter()
                .zip(fixture.topology.components(physical))
            {
                insert(&mut fixture.components, alias, value.id())?;
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        for (component, endpoint) in self.routes {
            let component = *fixture.components.get(&component).ok_or_else(|| {
                ScriptError::new("device builder", "route references unknown component")
            })?;
            let endpoint = *fixture.endpoints.get(&endpoint).ok_or_else(|| {
                ScriptError::new("device builder", "route references unknown endpoint")
            })?;
            if !seen.insert(component)
                || fixture.topology.component(component).unwrap().keyboard()
                    != fixture
                        .topology
                        .physical_device(fixture.topology.endpoint(endpoint).unwrap().physical())
                        .unwrap()
                        .keyboard()
            {
                return Err(ScriptError::new(
                    "device builder",
                    "duplicate or cross-keyboard route",
                ));
            }
            fixture.routes.push((component, endpoint));
        }
        Ok(fixture)
    }
}
impl DeviceFixture {
    pub(crate) fn context(
        &self,
        topology: &Topology,
        primary: &str,
        selected: &[&str],
    ) -> Result<crate::backends::ConnectContext, ScriptError> {
        let primary = *self
            .endpoints
            .get(primary)
            .ok_or_else(|| ScriptError::new("device builder", "unknown primary endpoint alias"))?;
        let selected = selected
            .iter()
            .map(|alias| {
                self.endpoints.get(*alias).copied().ok_or_else(|| {
                    ScriptError::new("device builder", "unknown selected endpoint alias")
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let routes = self
            .routes
            .iter()
            .filter(|(_, endpoint)| selected.contains(endpoint))
            .copied()
            .collect();
        crate::backends::ConnectContext::new(topology, primary, selected, routes)
            .map_err(|error| ScriptError::new("device builder", error.to_string()))
    }
    pub(crate) fn revalidate(&mut self) -> Result<(), ScriptError> {
        for endpoint in self.endpoints.values() {
            self.topology
                .revalidate_endpoint(*endpoint)
                .map_err(|error| ScriptError::new("device builder", error.to_string()))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
