//! Support evidence, not observed values, command authorization or routing policy.
//!
//! One explicit keyboard/component scope has one authoritative replacement report.
//! Firmware constraints are interpreted by reporters, never by this generic cache.

use std::collections::{BTreeMap, BTreeSet};

use super::{
    ModelError, Result,
    connection::{ConnectionState, UnavailableReason},
    identity::{
        AttachmentGeneration, CapabilityRefreshId, ComponentId, EndpointId, KeyboardId, nonblank,
    },
    topology::Topology,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum CapabilityScope {
    Keyboard(KeyboardId),
    Component(ComponentId),
}

/// Closed operations avoid nonsensical combinations such as writing a battery
/// reading. These names do not define command payloads or promise implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum CapabilityOperation {
    ReadIndicators,
    WriteIndicators,
    ReadLayers,
    ActivateLayer,
    ReadLighting,
    WriteLighting,
    ReadBattery,
    ReadWirelessState,
    ReadKeymaps,
    WriteKeymaps,
    ReadMacros,
    WriteMacros,
    SubscribeRuntimeEvents,
    ReadFirmware,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OperationAccess {
    ReadOnly,
    Writable,
}

impl CapabilityOperation {
    pub(crate) fn access(self) -> OperationAccess {
        match self {
            Self::WriteIndicators
            | Self::ActivateLayer
            | Self::WriteLighting
            | Self::WriteKeymaps
            | Self::WriteMacros => OperationAccess::Writable,
            // Subscription and outbound status requests do not mutate keyboard
            // configuration; transport write permissions are a different concern.
            Self::ReadIndicators
            | Self::ReadLayers
            | Self::ReadLighting
            | Self::ReadBattery
            | Self::ReadWirelessState
            | Self::ReadKeymaps
            | Self::ReadMacros
            | Self::SubscribeRuntimeEvents
            | Self::ReadFirmware => OperationAccess::ReadOnly,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Support {
    #[default]
    Unknown,
    Supported,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum VersionKind {
    Firmware,
    Protocol,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReportedVersion {
    kind: VersionKind,
    namespace: String,
    value: String,
}

impl ReportedVersion {
    pub(crate) fn new(
        kind: VersionKind,
        namespace: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<Self> {
        Ok(Self {
            kind,
            namespace: nonblank(namespace.into(), "version namespace")?,
            value: nonblank(value.into(), "reported version")?,
        })
    }
    pub(crate) fn kind(&self) -> VersionKind {
        self.kind
    }
    pub(crate) fn namespace(&self) -> &str {
        &self.namespace
    }
    pub(crate) fn value(&self) -> &str {
        &self.value
    }
}

/// An opaque expression in the reporter's language, not a core SemVer predicate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CapabilityConstraint {
    namespace: String,
    expression: String,
}

impl CapabilityConstraint {
    pub(crate) fn new(namespace: impl Into<String>, expression: impl Into<String>) -> Result<Self> {
        Ok(Self {
            namespace: nonblank(namespace.into(), "constraint namespace")?,
            expression: nonblank(expression.into(), "constraint expression")?,
        })
    }
    pub(crate) fn namespace(&self) -> &str {
        &self.namespace
    }
    pub(crate) fn expression(&self) -> &str {
        &self.expression
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CapabilityEntry {
    support: Support,
    constraints: Vec<CapabilityConstraint>,
}

impl CapabilityEntry {
    /// Support is the reporter's resolved answer in this context. An unresolved
    /// constraint must be reported as Unknown, not a conditional Supported entry.
    pub(crate) fn new(support: Support, constraints: Vec<CapabilityConstraint>) -> Self {
        Self {
            support,
            constraints,
        }
    }
    pub(crate) fn support(&self) -> Support {
        self.support
    }
    pub(crate) fn constraints(&self) -> &[CapabilityConstraint] {
        &self.constraints
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CapabilityReport {
    reporter: String,
    versions: Vec<ReportedVersion>,
    operations: BTreeMap<CapabilityOperation, CapabilityEntry>,
}

impl CapabilityReport {
    pub(crate) fn new(
        reporter: impl Into<String>,
        versions: Vec<ReportedVersion>,
        operations: Vec<(CapabilityOperation, CapabilityEntry)>,
    ) -> Result<Self> {
        let reporter = nonblank(reporter.into(), "capability reporter")?;
        let mut version_keys = BTreeSet::new();
        for version in &versions {
            if !version_keys.insert((version.kind, &version.namespace)) {
                return Err(ModelError::DuplicateCapabilityVersion {
                    kind: version.kind,
                    namespace: version.namespace.clone(),
                });
            }
        }
        let mut entries = BTreeMap::new();
        for (operation, entry) in operations {
            if entries.insert(operation, entry).is_some() {
                return Err(ModelError::DuplicateCapability(operation));
            }
        }
        Ok(Self {
            reporter,
            versions,
            operations: entries,
        })
    }
    pub(crate) fn reporter(&self) -> &str {
        &self.reporter
    }
    pub(crate) fn versions(&self) -> &[ReportedVersion] {
        &self.versions
    }
    pub(crate) fn entry(&self, operation: CapabilityOperation) -> Option<&CapabilityEntry> {
        self.operations.get(&operation)
    }
    pub(crate) fn operations(
        &self,
    ) -> impl Iterator<Item = (CapabilityOperation, &CapabilityEntry)> {
        self.operations
            .iter()
            .map(|(operation, entry)| (*operation, entry))
    }
    pub(crate) fn support(&self, operation: CapabilityOperation) -> Support {
        self.entry(operation)
            .map_or(Support::Unknown, |entry| entry.support)
    }
}

/// Also serves as accepted-report provenance. The unique refresh ID prevents
/// pending work from reviving after invalidation or same-attachment supersession.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CapabilityRefreshToken {
    refresh: CapabilityRefreshId,
    scope: CapabilityScope,
    keyboard: KeyboardId,
    source: EndpointId,
    attachment: AttachmentGeneration,
}

impl CapabilityRefreshToken {
    pub(crate) fn id(self) -> CapabilityRefreshId {
        self.refresh
    }
    pub(crate) fn scope(self) -> CapabilityScope {
        self.scope
    }
    pub(crate) fn source(self) -> EndpointId {
        self.source
    }
    pub(crate) fn attachment(self) -> AttachmentGeneration {
        self.attachment
    }
    fn stale(self) -> ModelError {
        ModelError::StaleCapabilityRefresh {
            refresh: self.refresh,
            scope: self.scope,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CapabilityDiscovery {
    NotDiscovered,
    Refreshing,
    Reported,
}

/// Availability describes the evidence source, independently of support. It
/// does not authorize a particular command or resolve backend routing policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceAvailability {
    Unknown,
    Available,
    Unavailable(UnavailableReason),
}

#[derive(Debug)]
pub(crate) struct CapabilityView<'a> {
    discovery: CapabilityDiscovery,
    report: Option<&'a CapabilityReport>,
    provenance: Option<CapabilityRefreshToken>,
    source_availability: SourceAvailability,
}

impl<'a> CapabilityView<'a> {
    pub(crate) fn discovery(&self) -> CapabilityDiscovery {
        self.discovery
    }
    pub(crate) fn report(&self) -> Option<&'a CapabilityReport> {
        self.report
    }
    pub(crate) fn provenance(&self) -> Option<CapabilityRefreshToken> {
        self.provenance
    }
    pub(crate) fn source_availability(&self) -> SourceAvailability {
        self.source_availability
    }
    pub(crate) fn support(&self, operation: CapabilityOperation) -> Support {
        self.report
            .map_or(Support::Unknown, |report| report.support(operation))
    }
}

#[derive(Debug, PartialEq, Eq)]
enum CapabilitySlot {
    Refreshing(CapabilityRefreshToken),
    Reported {
        provenance: CapabilityRefreshToken,
        report: CapabilityReport,
    },
}

impl CapabilitySlot {
    fn token(&self) -> CapabilityRefreshToken {
        match self {
            Self::Refreshing(token) => *token,
            Self::Reported { provenance, .. } => *provenance,
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct CapabilityCache {
    slots: BTreeMap<CapabilityScope, CapabilitySlot>,
}

impl CapabilityCache {
    pub(super) fn invalidate_keyboard(&mut self, keyboard: KeyboardId) {
        self.slots
            .retain(|_, slot| slot.token().keyboard != keyboard);
    }
    pub(super) fn cancel_pending_from(&mut self, source: EndpointId) {
        self.slots.retain(
            |_, slot| !matches!(slot, CapabilitySlot::Refreshing(token) if token.source == source),
        );
    }
    fn is_pending(&self, token: CapabilityRefreshToken) -> bool {
        matches!(self.slots.get(&token.scope), Some(CapabilitySlot::Refreshing(current)) if *current == token)
    }
}

impl Topology {
    fn capability_keyboard(&self, scope: CapabilityScope) -> Result<KeyboardId> {
        match scope {
            CapabilityScope::Keyboard(id) => {
                self.keyboard(id)?;
                Ok(id)
            }
            CapabilityScope::Component(id) => Ok(self.component(id)?.keyboard()),
        }
    }

    fn capability_source(
        &self,
        scope: CapabilityScope,
        source: EndpointId,
    ) -> Result<(KeyboardId, AttachmentGeneration)> {
        let keyboard = self.capability_keyboard(scope)?;
        let endpoint = self.endpoint(source)?;
        if self.physical_device(endpoint.physical())?.keyboard() != keyboard {
            return Err(ModelError::CapabilitySourceMismatch {
                endpoint: source,
                scope,
            });
        }
        let ConnectionState::Ready(attachment) = endpoint.state() else {
            return Err(ModelError::EndpointNotReady(source));
        };
        Ok((keyboard, attachment))
    }

    pub(crate) fn capabilities(&self, scope: CapabilityScope) -> Result<CapabilityView<'_>> {
        self.capability_keyboard(scope)?;
        let mut view = CapabilityView {
            discovery: CapabilityDiscovery::NotDiscovered,
            report: None,
            provenance: None,
            source_availability: SourceAvailability::Unknown,
        };
        match self.capabilities.slots.get(&scope) {
            Some(CapabilitySlot::Refreshing(_)) => view.discovery = CapabilityDiscovery::Refreshing,
            Some(CapabilitySlot::Reported { provenance, report }) => {
                view.discovery = CapabilityDiscovery::Reported;
                view.report = Some(report);
                view.provenance = Some(*provenance);
                view.source_availability = match self.endpoint(provenance.source)?.state() {
                    ConnectionState::Ready(generation) if generation == provenance.attachment => {
                        SourceAvailability::Available
                    }
                    ConnectionState::Unavailable(reason) => SourceAvailability::Unavailable(reason),
                    _ => SourceAvailability::Unknown,
                };
            }
            None => {}
        }
        Ok(view)
    }

    pub(crate) fn begin_capability_refresh(
        &mut self,
        scope: CapabilityScope,
        source: EndpointId,
    ) -> Result<CapabilityRefreshToken> {
        let (keyboard, attachment) = self.capability_source(scope, source)?;
        let refresh = CapabilityRefreshId::allocate()?;
        let token = CapabilityRefreshToken {
            refresh,
            scope,
            keyboard,
            source,
            attachment,
        };
        // Full replacement starts now, not when an arbitrarily slow query ends.
        self.capabilities
            .slots
            .insert(scope, CapabilitySlot::Refreshing(token));
        Ok(token)
    }

    pub(crate) fn complete_capability_refresh(
        &mut self,
        token: CapabilityRefreshToken,
        report: CapabilityReport,
    ) -> Result<()> {
        if !self.capabilities.is_pending(token)
            || !self
                .capability_source(token.scope, token.source)
                .is_ok_and(|context| context == (token.keyboard, token.attachment))
        {
            return Err(token.stale());
        }
        // Reports are immutable validated values. Validation and publication share
        // this exclusive topology borrow, leaving no check/insert race for callers.
        self.capabilities.slots.insert(
            token.scope,
            CapabilitySlot::Reported {
                provenance: token,
                report,
            },
        );
        Ok(())
    }

    pub(crate) fn abandon_capability_refresh(
        &mut self,
        token: CapabilityRefreshToken,
    ) -> Result<()> {
        if !self.capabilities.is_pending(token) {
            return Err(token.stale());
        }
        self.capabilities.slots.remove(&token.scope);
        Ok(())
    }

    /// Call when a reporter learns firmware/protocol context changed even if the
    /// device never disconnected. Opaque versions cannot be compared by the core.
    pub(crate) fn invalidate_capabilities(&mut self, keyboard: KeyboardId) -> Result<()> {
        self.keyboard(keyboard)?;
        self.capabilities.invalidate_keyboard(keyboard);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
