//! Snapshot queries and evidence-qualified CLI identities, never hardware pairing authority.
use crate::{
    error::{Error, Result},
    input::classification::{self, Admission, CollectionScope},
    model::{
        identity::{Identification, InputEndpointId},
        input::*,
        observation::{HostLock, Observation},
        state::*,
    },
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Selector {
    pub(crate) id: String,
    pub(crate) kind: &'static str,
    pub(crate) verification: &'static str,
    pub(crate) ambiguous: bool,
    pub(crate) fallback: String,
    pub(crate) fallback_ambiguous: bool,
    pub(crate) reason: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LockState {
    Known,
    Unknown,
    Unsupported,
    Stale,
    PermissionDenied,
    Unavailable,
    Error,
    NotCollected,
    Disagreement,
    Mixed,
}
impl LockState {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Known => "known",
            Self::Unknown => "unknown",
            Self::Unsupported => "unsupported",
            Self::Stale => "stale",
            Self::PermissionDenied => "permission denied",
            Self::Unavailable => "unavailable",
            Self::Error => "error",
            Self::NotCollected => "not collected",
            Self::Disagreement => "disagreement",
            Self::Mixed => "mixed",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub(crate) struct LockObservation {
    pub(crate) state: LockState,
    // Historical report values are deliberately absent here: they cannot count as current agreement.
    pub(crate) value: Option<bool>,
}
impl LockObservation {
    pub(crate) fn label(self) -> &'static str {
        match (self.state, self.value) {
            (LockState::Known, Some(true)) => "on",
            (LockState::Known, Some(false)) => "off",
            (LockState::Known, None) => "unknown",
            _ => self.state.label(),
        }
    }
}
#[derive(Clone, Debug, Serialize)]
pub(crate) struct LockSummary {
    #[serde(flatten)]
    pub(crate) observation: LockObservation,
    pub(crate) contributing: Vec<LockObservation>,
}
impl LockSummary {
    pub(crate) fn aggregate(observations: impl Iterator<Item = LockObservation>) -> Self {
        let contributing: Vec<_> = observations.collect::<BTreeSet<_>>().into_iter().collect();
        let known: BTreeSet<_> = contributing
            .iter()
            .filter(|o| o.state == LockState::Known)
            .map(|o| o.value)
            .collect();
        let observation = if contributing.is_empty() {
            LockObservation {
                state: LockState::NotCollected,
                value: None,
            }
        } else if contributing.len() == 1 {
            contributing[0]
        } else {
            LockObservation {
                state: if known.len() > 1 {
                    LockState::Disagreement
                } else {
                    LockState::Mixed
                },
                value: None,
            }
        };
        Self {
            observation,
            contributing,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct EndpointView {
    pub(crate) endpoint: InputEndpoint,
    pub(crate) host: Option<HostState>,
    // Uncertain candidates contribute missing knowledge, even though collection cannot open them anew.
    pub(crate) applicable: bool,
}
impl EndpointView {
    pub(crate) fn observation(&self, lock: HostLock) -> LockObservation {
        let Some(host) = &self.host else {
            return LockObservation {
                state: LockState::NotCollected,
                value: None,
            };
        };
        if let SourceAvailability::Unavailable(cause) = host.availability {
            return unavailable(cause);
        }
        let Some(field) = host.fields.get(&lock) else {
            return LockObservation {
                state: LockState::Unknown,
                value: None,
            };
        };
        if let Some(failure) = field.failure {
            return unavailable(failure.cause);
        }
        let Some(report) = &field.report else {
            return LockObservation {
                state: LockState::Unknown,
                value: None,
            };
        };
        if matches!(report.validity, Validity::Stale { .. }) {
            return LockObservation {
                state: LockState::Stale,
                value: None,
            };
        }
        match report.value {
            Observation::Known(value) => LockObservation {
                state: LockState::Known,
                value: Some(value),
            },
            Observation::Unknown => LockObservation {
                state: LockState::Unknown,
                value: None,
            },
            Observation::Unavailable(_) => LockObservation {
                state: LockState::Unavailable,
                value: None,
            },
        }
    }
}
fn unavailable(cause: SourceFailure) -> LockObservation {
    LockObservation {
        state: match cause {
            SourceFailure::PermissionDenied => LockState::PermissionDenied,
            SourceFailure::HostUnsupported => LockState::Unsupported,
            SourceFailure::Protocol | SourceFailure::InvalidBinding => LockState::Error,
            _ => LockState::Unavailable,
        },
        value: None,
    }
}

#[derive(Clone, Debug)]
pub(crate) struct DeviceView {
    pub(crate) group: InputDevice,
    pub(crate) name: String,
    pub(crate) selector: Selector,
    pub(crate) grouping: &'static str,
    pub(crate) class: InputClass,
    pub(crate) origin: InputOrigin,
    pub(crate) location: InputLocation,
    pub(crate) endpoints: Vec<EndpointView>,
}
impl DeviceView {
    pub(crate) fn lock(&self, lock: HostLock) -> LockSummary {
        LockSummary::aggregate(
            self.endpoints
                .iter()
                .filter(|e| e.applicable || e.host.is_some())
                .map(|e| e.observation(lock)),
        )
    }
    pub(crate) fn has_indicators(&self) -> bool {
        self.endpoints
            .iter()
            .any(|e| e.applicable || e.host.is_some())
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Catalog {
    pub(crate) devices: Vec<DeviceView>,
}
pub(crate) struct Selection<'a> {
    pub(crate) device: &'a DeviceView,
    pub(crate) endpoint: Option<InputEndpointId>,
}
impl Catalog {
    pub(crate) fn new(snapshot: &Snapshot) -> Self {
        Self::with_digest(snapshot, digest)
    }
    fn with_digest(snapshot: &Snapshot, hash: impl Fn(&[u8]) -> String) -> Self {
        let admissions = classification::admissions(
            snapshot.records.values().filter_map(|r| match r.as_ref() {
                StateRecord::InputEndpoint(endpoint) => Some(endpoint.as_ref()),
                _ => None,
            }),
            CollectionScope::Keyboards,
        );
        let mut devices = vec![];
        for record in snapshot.records.values() {
            let StateRecord::InputDevice(group) = record.as_ref() else {
                continue;
            };
            let mut endpoints: Vec<_> = snapshot
                .records
                .values()
                .filter_map(|r| match r.as_ref() {
                    StateRecord::InputEndpoint(endpoint) if endpoint.device == group.id => {
                        let host = snapshot.records.values().find_map(|r| match r.as_ref() {
                            StateRecord::Host(host) if host.input == Some(endpoint.id) => {
                                Some(host.as_ref().clone())
                            }
                            _ => None,
                        });
                        Some(EndpointView {
                            endpoint: endpoint.as_ref().clone(),
                            host,
                            applicable: admissions[&endpoint.id] != Admission::Deny,
                        })
                    }
                    _ => None,
                })
                .collect();
            endpoints.sort_by(|a, b| a.endpoint.descriptor.key.cmp(&b.endpoint.descriptor.key));
            let name =
                endpoints
                    .iter()
                    .filter(|e| e.endpoint.descriptor.classification.kind == InputClass::Keyboard)
                    .chain(endpoints.iter().filter(|e| {
                        e.endpoint.descriptor.classification.kind == InputClass::Partial
                    }))
                    .chain(endpoints.iter())
                    .filter_map(|e| e.endpoint.descriptor.name.known())
                    .find(|name| !name.trim().is_empty())
                    .cloned()
                    .unwrap_or_else(|| match &group.descriptor.identification {
                        Identification::Identified(model) => model.clone(),
                        _ => "Input device".into(),
                    });
            let grouping = if endpoints
                .iter()
                .any(|e| !e.endpoint.descriptor.group_confirmed)
            {
                "uncertain_attachment"
            } else if group.descriptor.usb.known().is_some() {
                "usb_attachment"
            } else {
                "input_attachment"
            };
            let class =
                classification::group_class(endpoints.iter().map(|e| &e.endpoint.descriptor));
            let relevant: Vec<_> = endpoints
                .iter()
                .filter(|e| {
                    matches!(
                        e.endpoint.descriptor.classification.kind,
                        InputClass::Keyboard | InputClass::Partial
                    )
                })
                .collect();
            let placement_endpoints = if relevant.is_empty() {
                endpoints.iter().collect::<Vec<_>>()
            } else {
                relevant
            };
            let origin = placement_endpoints
                .first()
                .map(|e| e.endpoint.descriptor.placement.origin)
                .filter(|origin| {
                    placement_endpoints
                        .iter()
                        .all(|e| e.endpoint.descriptor.placement.origin == *origin)
                })
                .unwrap_or(InputOrigin::Unknown);
            let location = placement_endpoints
                .first()
                .map(|e| e.endpoint.descriptor.placement.location)
                .filter(|location| {
                    placement_endpoints
                        .iter()
                        .all(|e| e.endpoint.descriptor.placement.location == *location)
                })
                .unwrap_or(InputLocation::Unknown);
            devices.push(DeviceView {
                group: group.as_ref().clone(),
                name,
                selector: selector(group, &hash),
                grouping,
                class,
                origin,
                location,
                endpoints,
            });
        }
        // Even collisions of shortened digests remain ambiguous, never accidental authority to pick a device.
        let mut counts = BTreeMap::<String, BTreeSet<_>>::new();
        for device in &devices {
            for selector in [&device.selector.id, &device.selector.fallback] {
                counts
                    .entry(selector.clone())
                    .or_default()
                    .insert(device.group.id);
            }
        }
        for device in &mut devices {
            device.selector.ambiguous = counts[&device.selector.id].len() > 1;
            device.selector.fallback_ambiguous = counts[&device.selector.fallback].len() > 1;
        }
        devices.sort_by(|a, b| {
            (&a.selector.id, &a.group.descriptor.key)
                .cmp(&(&b.selector.id, &b.group.descriptor.key))
        });
        Self { devices }
    }

    pub(crate) fn set_collection_scope(&mut self, scope: CollectionScope) {
        let admissions = classification::admissions(
            self.devices
                .iter()
                .flat_map(|d| d.endpoints.iter().map(|e| &e.endpoint)),
            scope,
        );
        for endpoint in self.devices.iter_mut().flat_map(|d| &mut d.endpoints) {
            endpoint.applicable = admissions[&endpoint.endpoint.id] != Admission::Deny;
        }
    }

    pub(crate) fn resolve(&self, query: &Path, allow_paths: bool) -> Result<Selection<'_>> {
        let text = query.to_str();
        let mut matches = vec![];
        for device in &self.devices {
            if text.is_some_and(|s| s == device.selector.id || s == device.selector.fallback)
                || (allow_paths && device.group.descriptor.key.0 == query)
            {
                matches.push(Selection {
                    device,
                    endpoint: None,
                });
            }
            if allow_paths {
                for endpoint in &device.endpoints {
                    let d = &endpoint.endpoint.descriptor;
                    if d.key.0 == query || d.devnode.known().is_some_and(|p| p == query) {
                        matches.push(Selection {
                            device,
                            endpoint: Some(endpoint.endpoint.id),
                        });
                    }
                }
            }
        }
        if matches.len() == 1 {
            return Ok(matches.remove(0));
        }
        let candidates = if matches.is_empty() {
            self.devices.iter().collect::<Vec<_>>()
        } else {
            matches.iter().map(|m| m.device).collect()
        };
        Err(Error::Selection {
            selector: query.into(),
            matches: matches.len(),
            candidates: candidates
                .iter()
                .map(|d| {
                    format!(
                        "  {} (attachment fallback: {}; locator: {})",
                        d.selector.id,
                        d.selector.fallback,
                        d.group.descriptor.key.0.display()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"),
        })
    }
}

fn selector(group: &InputDevice, hash: &impl Fn(&[u8]) -> String) -> Selector {
    let path = group.descriptor.key.0.as_os_str().as_encoded_bytes();
    let fallback = format!(
        "attachment-{}",
        hash(&encode(b"kbctl/selector/v1/attachment", &[path]))
    );
    let identity = group
        .descriptor
        .usb
        .known()
        .zip(group.descriptor.serial.known())
        .filter(|(_, serial)| !serial.trim().is_empty());
    let (id, kind, verification, reason) = if let Some((usb, serial)) = identity {
        let encoded = encode(
            b"kbctl/selector/v1/usb",
            &[
                &usb.vendor.to_be_bytes(),
                &usb.product.to_be_bytes(),
                serial.as_bytes(),
            ],
        );
        (
            format!("usb-{}", hash(&encoded)),
            "usb_serial",
            "advertised",
            "usb_ids_and_advertised_serial",
        )
    } else {
        (
            fallback.clone(),
            "attachment",
            "unverified",
            "insufficient_stable_identity_evidence",
        )
    };
    Selector {
        id,
        kind,
        verification,
        ambiguous: false,
        fallback,
        fallback_ambiguous: false,
        reason,
    }
}
fn encode(namespace: &[u8], fields: &[&[u8]]) -> Vec<u8> {
    let mut bytes = namespace.to_vec();
    bytes.push(0);
    // Length prefixes prevent concatenation ambiguity without normalizing hardware-advertised text.
    for field in fields {
        bytes.extend_from_slice(&(field.len() as u64).to_be_bytes());
        bytes.extend_from_slice(field);
    }
    bytes
}
fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests;
