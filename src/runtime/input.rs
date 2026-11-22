//! Stage input replacement before touching the exclusive runtime's published state.
use super::*;
use crate::model::{
    identity::{InputDeviceId, InputEndpointId},
    input::*,
};

#[derive(Debug, Error, Diagnostic)]
pub(crate) enum InputError {
    #[error("invalid input inventory: {0}")]
    #[diagnostic(
        code(kbctl::input::invalid_inventory),
        help(
            "Rescan input metadata and supply distinct live endpoints with consistent attachment groups."
        )
    )]
    Invalid(&'static str),
    #[error("input attachment group {0} is no longer present")]
    #[diagnostic(
        code(kbctl::input::unknown_device),
        help("Refresh the input snapshot and accept an association using current IDs.")
    )]
    Unknown(InputDeviceId),
}

#[derive(Clone, Default)]
pub(super) struct Inventory {
    groups: BTreeMap<DiscoveryKey, InputDevice>,
    endpoints: BTreeMap<DiscoveryKey, InputEndpoint>,
    status: DiscoveryStatus,
    // Absence before collection should not change existing runtimes' bootstrap snapshots.
    started: bool,
}

fn same_instance(old: &Option<Incarnation>, new: &Option<Incarnation>, continuous: bool) -> bool {
    match (old, new) {
        (Some(a), Some(b)) => a == b,
        _ => continuous,
    }
}

impl Inventory {
    fn staged(&self, mut scan: Scan, monitor: MonitorStatus) -> Result<Self> {
        let continuous = matches!(self.status.monitor, MonitorStatus::Watching)
            && matches!(monitor, MonitorStatus::Watching)
            && matches!(self.status.inventory, InventoryStatus::Current);
        let mut next = self.clone();
        next.started = true;
        next.status = DiscoveryStatus {
            inventory: if scan.complete && scan.problems.is_empty() {
                InventoryStatus::Current
            } else {
                InventoryStatus::Degraded(scan.problems.clone())
            },
            monitor,
        };
        // Partial membership is not evidence of removal, even if its vector happens to be empty.
        if !scan.complete {
            return Ok(next);
        }
        // Validate the source graph before uncertainty repair can discard unused fallback groups.
        let source_groups: std::collections::BTreeSet<_> =
            scan.groups.iter().map(|group| &group.key).collect();
        if source_groups.len() != scan.groups.len()
            || source_groups.iter().any(|key| key.0.as_os_str().is_empty())
        {
            return Err(InputError::Invalid("empty or duplicate source group key").into());
        }
        if scan
            .endpoints
            .iter()
            .any(|endpoint| !source_groups.contains(&endpoint.group))
        {
            return Err(InputError::Invalid("source endpoint has no group").into());
        }
        // A failed ancestry/bus read is not proof that a live endpoint moved to another attachment.
        let mut uncertain_fallbacks = std::collections::BTreeSet::new();
        for endpoint in &mut scan.endpoints {
            if !endpoint.group_confirmed
                && let Some(old) = self.endpoints.get(&endpoint.key).filter(|old| {
                    same_instance(
                        &old.descriptor.incarnation,
                        &endpoint.incarnation,
                        continuous,
                    )
                })
                && let Some(group) = self.groups.values().find(|group| group.id == old.device)
            {
                uncertain_fallbacks.insert(endpoint.group.clone());
                endpoint.group = group.descriptor.key.clone();
                if !scan
                    .groups
                    .iter()
                    .any(|candidate| candidate.key == endpoint.group)
                {
                    let mut descriptor = group.descriptor.clone();
                    // Retain attachment continuity, not an unrefreshed registry model claim.
                    descriptor.usb = Metadata::Missing;
                    descriptor.serial = Metadata::Missing;
                    descriptor.identification = model::identity::Identification::Unknown;
                    scan.groups.push(descriptor);
                }
            }
        }
        scan.groups.retain(|group| {
            !uncertain_fallbacks.contains(&group.key)
                || scan
                    .endpoints
                    .iter()
                    .any(|endpoint| endpoint.group == group.key)
        });
        let mut groups = BTreeMap::new();
        for mut descriptor in scan.groups {
            if descriptor.key.0.as_os_str().is_empty() {
                return Err(InputError::Invalid("empty group key").into());
            }
            if groups.contains_key(&descriptor.key) {
                return Err(InputError::Invalid("duplicate group").into());
            }
            let old = self.groups.get(&descriptor.key).filter(|old| {
                same_instance(
                    &old.descriptor.incarnation,
                    &descriptor.incarnation,
                    continuous,
                )
            });
            // Temporary metadata loss must not discard a previously established live fingerprint.
            if descriptor.incarnation.is_none() {
                descriptor.incarnation = old.and_then(|old| old.descriptor.incarnation.clone());
            }
            let device = InputDevice {
                id: match old {
                    Some(old) => old.id,
                    None => InputDeviceId::new()?,
                },
                physical: old.and_then(|old| old.physical),
                descriptor,
            };
            groups.insert(device.descriptor.key.clone(), device);
        }
        let mut endpoints = BTreeMap::new();
        for mut descriptor in scan.endpoints {
            if descriptor.key.0.as_os_str().is_empty() {
                return Err(InputError::Invalid("empty endpoint key").into());
            }
            let group = groups
                .get(&descriptor.group)
                .ok_or(InputError::Invalid("endpoint has no group"))?;
            if endpoints.contains_key(&descriptor.key) {
                return Err(InputError::Invalid("duplicate endpoint").into());
            }
            let old = self.endpoints.get(&descriptor.key).filter(|old| {
                old.device == group.id
                    && same_instance(
                        &old.descriptor.incarnation,
                        &descriptor.incarnation,
                        continuous,
                    )
            });
            if descriptor.incarnation.is_none() {
                descriptor.incarnation = old.and_then(|old| old.descriptor.incarnation.clone());
            }
            let endpoint = InputEndpoint {
                id: match old {
                    Some(old) => old.id,
                    None => InputEndpointId::new()?,
                },
                device: group.id,
                descriptor,
            };
            endpoints.insert(endpoint.descriptor.key.clone(), endpoint);
        }
        if groups.values().any(|group| {
            !endpoints
                .values()
                .any(|endpoint| endpoint.device == group.id)
        }) {
            return Err(InputError::Invalid("group has no endpoint").into());
        }
        next.groups = groups;
        next.endpoints = endpoints;
        Ok(next)
    }

    pub(super) fn project(&mut self, topology: &Topology, records: &mut Records) {
        for group in self.groups.values_mut() {
            // Topology retirement and association cleanup become one observable commit.
            if group
                .physical
                .is_some_and(|id| topology.physical_device(id).is_err())
            {
                group.physical = None;
            }
            let record = StateRecord::InputDevice(Box::new(group.clone()));
            records.insert(record.key(), Arc::new(record));
        }
        for endpoint in self.endpoints.values() {
            let record = StateRecord::InputEndpoint(Box::new(endpoint.clone()));
            records.insert(record.key(), Arc::new(record));
        }
        if self.started {
            let record = StateRecord::InputDiscovery(self.status.clone());
            records.insert(record.key(), Arc::new(record));
        }
    }
}

impl Runtime {
    pub(crate) fn input_endpoints(&self) -> impl Iterator<Item = &InputEndpoint> {
        self.inputs.endpoints.values()
    }
    pub(crate) fn input_endpoint(&self, id: InputEndpointId) -> Option<&InputEndpoint> {
        self.input_endpoints().find(|endpoint| endpoint.id == id)
    }
    pub(crate) fn apply_input_scan(
        &mut self,
        scan: Scan,
        monitor: MonitorStatus,
        now: Instant,
    ) -> Result<CommitOutcome> {
        let next = self.inputs.staged(scan, monitor)?;
        let action = self.prepare(now)?;
        self.inputs = next;
        Ok(self.finish(action, 0))
    }

    pub(crate) fn associate_input_device(
        &mut self,
        id: InputDeviceId,
        physical: Option<PhysicalDeviceId>,
        now: Instant,
    ) -> Result<CommitOutcome> {
        if let Some(id) = physical {
            self.topology.physical_device(id)?;
        }
        let key = self
            .inputs
            .groups
            .iter()
            .find(|(_, group)| group.id == id)
            .map(|(key, _)| key.clone())
            .ok_or(InputError::Unknown(id))?;
        let action = self.prepare(now)?;
        self.inputs
            .groups
            .get_mut(&key)
            .expect("validated input group")
            .physical = physical;
        Ok(self.finish(action, 0))
    }
}

#[cfg(test)]
mod tests;
