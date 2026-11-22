//! Pure discovery classification and proposed routes, never live device authority.

// The host will consume these private records after enumeration is implemented.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};

use miette::Diagnostic;
use thiserror::Error;

use crate::{
    backends::{Backend, BackendCandidate, DiscoveryView, MatchEvidence},
    model::{
        connection::TransportKind,
        identity::{AdvertisedSerial, EndpointId, InterfaceId, KeyboardId, PhysicalDeviceId},
        topology::{ComponentKind, HidMetadata, Topology, UsbIds},
    },
};

pub(crate) mod builtin;
mod matching;

pub(crate) use crate::model::identity::Identification;

#[derive(Debug, Clone, Copy)]
pub(crate) struct PhysicalMetadata<'a> {
    pub(crate) usb: Option<UsbIds>,
    pub(crate) serial: Option<&'a AdvertisedSerial>,
}

pub(crate) struct MetadataIdentification {
    pub(crate) identification: Identification,
    pub(crate) assessments: Vec<DefinitionAssessment>,
}

#[derive(Debug, Error, Diagnostic)]
pub(crate) enum RegistryError {
    #[error("invalid registry declaration `{id}`: {reason}")]
    #[diagnostic(
        code(kbctl::registry::invalid_definition),
        help(
            "Correct the declaration and its source/template/role references before registering it."
        )
    )]
    InvalidDefinition { id: String, reason: String },
    #[error("backend catalog repeats implementation identity `{0}`")]
    #[diagnostic(
        code(kbctl::registry::duplicate_backend),
        help(
            "Register one implementation per backend identity; resolve configuration alternatives through explicit mappings."
        )
    )]
    DuplicateBackend(String),
    #[error("backend `{backend}` returned an inconsistent metadata candidate: {reason}")]
    #[diagnostic(
        code(kbctl::registry::backend_contract),
        help(
            "Fix the matcher to return only the selected configuration, owned endpoints and actual discovery evidence."
        )
    )]
    BackendContract {
        backend: String,
        reason: &'static str,
    },
    #[error("cannot inspect discovered physical device")]
    #[diagnostic(
        code(kbctl::registry::discovery),
        help("Refresh discovery and inspect a live physical-device instance.")
    )]
    Discovery(#[from] crate::backends::error::BackendError),
}
pub(crate) type Result<T> = std::result::Result<T, RegistryError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceClass {
    Vendor,
    Upstream,
    Community,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceRecord {
    pub(crate) id: String,
    pub(crate) class: SourceClass,
    pub(crate) reference: String,
    pub(crate) scope: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeviceMode {
    Runtime,
    Bootloader,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PhysicalSelector {
    pub(crate) usb: UsbIds,
    pub(crate) serial: Option<AdvertisedSerial>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EndpointSelector {
    pub(crate) transport: TransportKind,
    pub(crate) interface_number: Option<u8>,
    pub(crate) hid: HidMetadata,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EndpointRole {
    pub(crate) key: String,
    pub(crate) selector: EndpointSelector,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MappingStatus {
    ResearchOnly,
    Enabled,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProtocolReference {
    pub(crate) id: String,
    pub(crate) backend: String,
    pub(crate) configuration: String,
    pub(crate) status: MappingStatus,
    pub(crate) primary_role: String,
    pub(crate) roles: Vec<EndpointRole>,
    pub(crate) sources: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ComponentTemplate {
    pub(crate) key: String,
    pub(crate) kind: ComponentKind,
    pub(crate) parent: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeviceDefinition {
    pub(crate) id: String,
    pub(crate) model: String,
    pub(crate) family: String,
    pub(crate) variant: String,
    pub(crate) mode: DeviceMode,
    pub(crate) selector: PhysicalSelector,
    pub(crate) components: Vec<ComponentTemplate>,
    pub(crate) protocols: Vec<ProtocolReference>,
    pub(crate) sources: Vec<String>,
    pub(crate) limitations: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Fact {
    Usb(UsbIds),
    Serial(AdvertisedSerial),
    Transport(TransportKind),
    Interface(u8),
    UsagePage(u16),
    Usage(u16),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    Satisfied,
    Missing,
    Mismatch,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PredicateCheck {
    pub(crate) expected: Fact,
    pub(crate) observed: Option<Fact>,
}
impl PredicateCheck {
    pub(crate) fn verdict(&self) -> Verdict {
        match &self.observed {
            None => Verdict::Missing,
            Some(fact) if *fact == self.expected => Verdict::Satisfied,
            Some(_) => Verdict::Mismatch,
        }
    }
    fn evidence(&self) -> Option<MatchEvidence> {
        if self.verdict() != Verdict::Satisfied {
            return None;
        }
        Some(match &self.expected {
            Fact::Usb(usb) => MatchEvidence::UsbModel(*usb),
            Fact::Serial(serial) => MatchEvidence::AdvertisedSerial(serial.clone()),
            Fact::Transport(kind) => MatchEvidence::Transport(*kind),
            Fact::Interface(number) => MatchEvidence::Interface(*number),
            Fact::UsagePage(page) => MatchEvidence::HidUsagePage(*page),
            Fact::Usage(usage) => MatchEvidence::HidUsage(*usage),
        })
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DefinitionAssessment {
    pub(crate) definition: String,
    pub(crate) checks: Vec<PredicateCheck>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EndpointAssessment {
    pub(crate) endpoint: EndpointId,
    pub(crate) checks: Vec<PredicateCheck>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RoleAssessment {
    pub(crate) role: String,
    pub(crate) matches: Vec<EndpointId>,
    pub(crate) endpoints: Vec<EndpointAssessment>,
}
pub(crate) type RoleAssignment = BTreeMap<String, EndpointId>;
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Assignment {
    NoEndpoints,
    NoMatch,
    Unique(RoleAssignment),
    Ambiguous {
        first: RoleAssignment,
        second: RoleAssignment,
    },
    Indeterminate,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RouteStatus {
    ResearchOnly,
    BackendUnavailable,
    Incomplete,
    Unresolved,
    Declined,
    Ready,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProtocolAssessment {
    pub(crate) reference: ProtocolReference,
    pub(crate) roles: Vec<RoleAssessment>,
    pub(crate) assignment: Assignment,
    pub(crate) routing: RouteStatus,
}

/// Expected component structure is not an assertion of observed presence. The
/// application must revalidate and explicitly accept plans before mutating state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AssociationPlan {
    pub(crate) physical: PhysicalDeviceId,
    pub(crate) keyboard: KeyboardId,
    pub(crate) interfaces: Vec<InterfaceId>,
    pub(crate) endpoints: Vec<EndpointId>,
    pub(crate) components: Vec<ComponentTemplate>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DefinitionMatch {
    pub(crate) definition: DeviceDefinition,
    pub(crate) protocols: Vec<ProtocolAssessment>,
    pub(crate) plan: AssociationPlan,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RegistryRoute {
    pub(crate) candidate: BackendCandidate,
    pub(crate) primary: EndpointId,
    pub(crate) roles: RoleAssignment,
    pub(crate) definitions: BTreeSet<String>,
    pub(crate) sources: BTreeSet<String>,
    pub(crate) references: BTreeSet<(String, String)>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Inspection {
    pub(crate) physical: PhysicalDeviceId,
    pub(crate) identification: Identification,
    pub(crate) assessments: Vec<DefinitionAssessment>,
    pub(crate) matches: Vec<DefinitionMatch>,
    pub(crate) routes: Vec<RegistryRoute>,
}
impl Inspection {
    pub(crate) fn actionable(&self) -> Option<&RegistryRoute> {
        if !matches!(self.identification, Identification::Identified(_))
            || self.routes.len() != 1
            || self
                .matches
                .iter()
                .flat_map(|matched| &matched.protocols)
                .any(|protocol| protocol.routing == RouteStatus::Unresolved)
        {
            return None;
        }
        self.routes.first()
    }
}

pub(crate) struct Registry {
    definitions: Vec<DeviceDefinition>,
    sources: BTreeMap<String, SourceRecord>,
    search_budget: usize,
}
fn invalid(id: &str, reason: impl Into<String>) -> RegistryError {
    RegistryError::InvalidDefinition {
        id: id.into(),
        reason: reason.into(),
    }
}
fn valid_id(id: &str) -> bool {
    !id.trim().is_empty() && id == id.trim()
}

impl Registry {
    pub(crate) fn new(
        mut definitions: Vec<DeviceDefinition>,
        sources: Vec<SourceRecord>,
        search_budget: usize,
    ) -> Result<Self> {
        if search_budget == 0 || search_budget > 100_000 {
            return Err(invalid(
                "registry",
                "assignment search budget must be in 1..=100000",
            ));
        }
        let mut source_map = BTreeMap::new();
        for source in sources {
            if !valid_id(&source.id)
                || source.reference.trim().is_empty()
                || source.scope.trim().is_empty()
            {
                return Err(invalid(
                    &source.id,
                    "source needs an identity, reference and scoped claim",
                ));
            }
            let id = source.id.clone();
            if source_map.insert(id.clone(), source).is_some() {
                return Err(invalid(&id, "duplicate source identity"));
            }
        }
        let mut ids = BTreeSet::new();
        for definition in &mut definitions {
            let id = &definition.id;
            if !valid_id(id) || !ids.insert(id.clone()) {
                return Err(invalid(id, "invalid or duplicate definition identity"));
            }
            if definition.model.trim().is_empty()
                || !valid_id(&definition.family)
                || definition.variant.trim().is_empty()
                || definition.limitations.trim().is_empty()
            {
                return Err(invalid(
                    id,
                    "model, family, variant and limitations must be explicit",
                ));
            }
            validate_sources(id, &mut definition.sources, &source_map)?;
            let templates: BTreeMap<_, _> = definition
                .components
                .iter()
                .map(|component| (&component.key, component))
                .collect();
            if templates.len() != definition.components.len() {
                return Err(invalid(id, "duplicate component template key"));
            }
            for component in &definition.components {
                if !valid_id(&component.key) {
                    return Err(invalid(id, "blank or padded component key"));
                }
                let mut visited = BTreeSet::from([component.key.as_str()]);
                let mut parent = component.parent.as_ref();
                while let Some(key) = parent {
                    let ancestor = templates
                        .get(key)
                        .ok_or_else(|| invalid(id, format!("unknown parent template `{key}`")))?;
                    if !visited.insert(key.as_str()) {
                        return Err(invalid(id, "component template cycle"));
                    }
                    parent = ancestor.parent.as_ref();
                }
            }
            if definition.mode == DeviceMode::Bootloader && !definition.protocols.is_empty() {
                return Err(invalid(
                    id,
                    "bootloader identification must not expose runtime protocol mappings",
                ));
            }
            let mut protocol_ids = BTreeSet::new();
            for protocol in &mut definition.protocols {
                // Bound recursion independently of the search work budget; a
                // malformed declaration must not turn metadata inspection into a stack overflow.
                if protocol.roles.is_empty() || protocol.roles.len() > 16 {
                    return Err(invalid(
                        id,
                        "a protocol needs 1..=16 distinct endpoint roles",
                    ));
                }
                if !valid_id(&protocol.id)
                    || !protocol_ids.insert(protocol.id.clone())
                    || !valid_id(&protocol.backend)
                    || !valid_id(&protocol.configuration)
                {
                    return Err(invalid(
                        id,
                        "invalid or duplicate protocol identity/configuration",
                    ));
                }
                validate_sources(id, &mut protocol.sources, &source_map)?;
                let mut roles = BTreeSet::new();
                for role in &protocol.roles {
                    if !valid_id(&role.key) || !roles.insert(role.key.clone()) {
                        return Err(invalid(id, "invalid or duplicate endpoint role"));
                    }
                    if role.selector.transport != TransportKind::Hid
                        && role.selector.hid != HidMetadata::default()
                    {
                        return Err(invalid(id, "HID predicates require a HID channel"));
                    }
                }
                if !roles.contains(&protocol.primary_role) {
                    return Err(invalid(
                        id,
                        "primary endpoint role must name a declared role",
                    ));
                }
                protocol.roles.sort_by(|a, b| a.key.cmp(&b.key));
            }
            definition.protocols.sort_by(|a, b| a.id.cmp(&b.id));
            definition.components.sort_by(|a, b| a.key.cmp(&b.key));
        }
        definitions.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(Self {
            definitions,
            sources: source_map,
            search_budget,
        })
    }
    pub(crate) fn definitions(&self) -> &[DeviceDefinition] {
        &self.definitions
    }
    pub(crate) fn identify_metadata(
        &self,
        metadata: PhysicalMetadata<'_>,
    ) -> MetadataIdentification {
        let assessments: Vec<_> = self
            .definitions
            .iter()
            .map(|definition| DefinitionAssessment {
                definition: definition.id.clone(),
                checks: matching::physical_checks(&definition.selector, metadata),
            })
            .collect();
        let identification = matching::identify(
            assessments
                .iter()
                .filter(|assessment| {
                    assessment
                        .checks
                        .iter()
                        .all(|check| check.verdict() == Verdict::Satisfied)
                })
                .map(|assessment| assessment.definition.clone())
                .collect(),
        );
        MetadataIdentification {
            identification,
            assessments,
        }
    }
    pub(crate) fn source(&self, id: &str) -> Option<&SourceRecord> {
        self.sources.get(id)
    }
    pub(crate) fn inspect(
        &self,
        topology: &Topology,
        physical: PhysicalDeviceId,
        backends: &[&dyn Backend],
    ) -> Result<Inspection> {
        matching::inspect(self, DiscoveryView::new(topology, physical)?, backends)
    }
}

fn validate_sources(
    id: &str,
    references: &mut Vec<String>,
    sources: &BTreeMap<String, SourceRecord>,
) -> Result<()> {
    if references.is_empty() || !references.iter().all(|key| sources.contains_key(key)) {
        return Err(invalid(
            id,
            "each declaration needs resolved source references",
        ));
    }
    references.sort();
    references.dedup();
    Ok(())
}

#[cfg(test)]
mod tests;
