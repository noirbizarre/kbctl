//! Validated ownership, explicit aggregation and bounded retirement operations.

use std::collections::{BTreeMap, BTreeSet};

use super::{
    ModelError, Result,
    capability::CapabilityCache,
    connection::{
        AttachmentToken, Connection, ConnectionState, EndpointLocator, OperationToken,
        TransportKind, UnavailableReason,
    },
    identity::{
        AdvertisedSerial, AttachmentGeneration, ComponentId, EndpointId, HardwareIdentity,
        IdentityComparison, InterfaceId, KeyboardId, MembershipGeneration, PhysicalDeviceId,
        nonblank,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DisplayName(String);

impl DisplayName {
    pub(crate) fn new(value: impl Into<String>) -> Result<Self> {
        nonblank(value.into(), "display name").map(Self)
    }
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
    fn or_fallback(name: Option<Self>, fallback: &str) -> Self {
        name.unwrap_or_else(|| Self(fallback.to_owned()))
    }
}

/// Model-family metadata, never an individual device's identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct UsbIds {
    pub(crate) vendor: u16,
    pub(crate) product: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Side {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ComponentKind {
    Body,
    Half(Option<Side>),
    Controller,
    Receiver,
    Dongle,
    Module,
}

impl ComponentKind {
    fn fallback(self) -> &'static str {
        match self {
            Self::Body => "Keyboard body",
            Self::Half(Some(Side::Left)) => "Left half",
            Self::Half(Some(Side::Right)) => "Right half",
            Self::Half(None) => "Keyboard half",
            Self::Controller => "Controller",
            Self::Receiver => "Receiver",
            Self::Dongle => "Dongle",
            Self::Module => "Module",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct LogicalKeyboard {
    id: KeyboardId,
    name: DisplayName,
}

impl LogicalKeyboard {
    pub(crate) fn id(&self) -> KeyboardId {
        self.id
    }
    pub(crate) fn name(&self) -> &DisplayName {
        &self.name
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PhysicalDevice {
    id: PhysicalDeviceId,
    keyboard: KeyboardId,
    membership: MembershipGeneration,
    name: DisplayName,
    identity: HardwareIdentity,
    usb_ids: Option<UsbIds>,
    serial: Option<AdvertisedSerial>,
}

impl PhysicalDevice {
    pub(crate) fn id(&self) -> PhysicalDeviceId {
        self.id
    }
    pub(crate) fn keyboard(&self) -> KeyboardId {
        self.keyboard
    }
    pub(crate) fn name(&self) -> &DisplayName {
        &self.name
    }
    pub(crate) fn identity(&self) -> &HardwareIdentity {
        &self.identity
    }
    pub(crate) fn usb_ids(&self) -> Option<UsbIds> {
        self.usb_ids
    }
    pub(crate) fn serial(&self) -> Option<&AdvertisedSerial> {
        self.serial.as_ref()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Interface {
    id: InterfaceId,
    physical: PhysicalDeviceId,
    name: DisplayName,
    usb_number: Option<u8>,
}

impl Interface {
    pub(crate) fn id(&self) -> InterfaceId {
        self.id
    }
    pub(crate) fn physical(&self) -> PhysicalDeviceId {
        self.physical
    }
    pub(crate) fn name(&self) -> &DisplayName {
        &self.name
    }
    pub(crate) fn usb_number(&self) -> Option<u8> {
        self.usb_number
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Endpoint {
    id: EndpointId,
    physical: PhysicalDeviceId,
    interface: Option<InterfaceId>,
    transport: TransportKind,
    locator: EndpointLocator,
    connection: Connection,
    hid: HidMetadata,
}

impl Endpoint {
    pub(crate) fn id(&self) -> EndpointId {
        self.id
    }
    pub(crate) fn physical(&self) -> PhysicalDeviceId {
        self.physical
    }
    pub(crate) fn interface(&self) -> Option<InterfaceId> {
        self.interface
    }
    pub(crate) fn transport(&self) -> TransportKind {
        self.transport
    }
    pub(crate) fn locator(&self) -> &EndpointLocator {
        &self.locator
    }
    pub(crate) fn state(&self) -> ConnectionState {
        self.connection.state()
    }
    pub(crate) fn hid(&self) -> HidMetadata {
        self.hid
    }
}

/// Facts about this addressable HID collection, never borrowed from another
/// collection on the same USB interface. Partial discovery stays explicit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct HidMetadata {
    pub(crate) usage_page: Option<u16>,
    pub(crate) usage: Option<u16>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Component {
    id: ComponentId,
    physical: PhysicalDeviceId,
    keyboard: KeyboardId,
    parent: Option<ComponentId>,
    kind: ComponentKind,
    name: DisplayName,
}

impl Component {
    pub(crate) fn id(&self) -> ComponentId {
        self.id
    }
    pub(crate) fn physical(&self) -> PhysicalDeviceId {
        self.physical
    }
    pub(crate) fn keyboard(&self) -> KeyboardId {
        self.keyboard
    }
    pub(crate) fn parent(&self) -> Option<ComponentId> {
        self.parent
    }
    pub(crate) fn kind(&self) -> ComponentKind {
        self.kind
    }
    pub(crate) fn name(&self) -> &DisplayName {
        &self.name
    }
}

/// References in specifications are local indexes, so assembly cannot accidentally
/// bind a channel or parent to a record from a different physical device.
#[derive(Debug)]
pub(crate) struct InterfaceSpec {
    pub(crate) name: Option<DisplayName>,
    pub(crate) usb_number: Option<u8>,
}

#[derive(Debug)]
pub(crate) struct EndpointSpec {
    pub(crate) interface: Option<usize>,
    pub(crate) transport: TransportKind,
    pub(crate) locator: EndpointLocator,
    pub(crate) hid: HidMetadata,
}

#[derive(Debug)]
pub(crate) struct ComponentSpec {
    pub(crate) name: Option<DisplayName>,
    pub(crate) kind: ComponentKind,
    pub(crate) parent: Option<usize>,
}

#[derive(Debug)]
pub(crate) struct PhysicalDeviceSpec {
    pub(crate) name: Option<DisplayName>,
    pub(crate) identity: HardwareIdentity,
    pub(crate) usb_ids: Option<UsbIds>,
    pub(crate) serial: Option<AdvertisedSerial>,
    pub(crate) interfaces: Vec<InterfaceSpec>,
    pub(crate) endpoints: Vec<EndpointSpec>,
    pub(crate) components: Vec<ComponentSpec>,
}

impl PhysicalDeviceSpec {
    fn validate(&self) -> Result<()> {
        let mut usb_numbers = BTreeSet::new();
        for interface in &self.interfaces {
            if let Some(number) = interface.usb_number
                && !usb_numbers.insert(number)
            {
                return Err(ModelError::DuplicateInterface { number });
            }
        }
        for (index, endpoint) in self.endpoints.iter().enumerate() {
            if endpoint.transport != TransportKind::Hid && endpoint.hid != HidMetadata::default() {
                return Err(ModelError::UnexpectedHidMetadata { index });
            }
            if let Some(index) = endpoint.interface
                && index >= self.interfaces.len()
            {
                return Err(ModelError::InvalidReference {
                    kind: "interface",
                    index,
                });
            }
        }
        for (index, component) in self.components.iter().enumerate() {
            let mut visited = BTreeSet::from([index]);
            let mut parent = component.parent;
            // Following ancestors permits forward references while rejecting both
            // self-parenting and longer cycles before publishing any records.
            while let Some(next) = parent {
                let ancestor = self
                    .components
                    .get(next)
                    .ok_or(ModelError::InvalidReference {
                        kind: "parent component",
                        index: next,
                    })?;
                if !visited.insert(next) {
                    return Err(ModelError::ComponentCycle { index: next });
                }
                parent = ancestor.parent;
            }
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum IdentityCandidates {
    InsufficientEvidence,
    NoMatch,
    One(PhysicalDeviceId),
    Ambiguous(Vec<PhysicalDeviceId>),
}

/// Not Clone: duplicating the aggregate would create two independently mutable
/// owners of the same runtime instances. Snapshots will be a separate concern.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Topology {
    pub(super) capabilities: CapabilityCache,
    keyboards: BTreeMap<KeyboardId, LogicalKeyboard>,
    physical: BTreeMap<PhysicalDeviceId, PhysicalDevice>,
    interfaces: BTreeMap<InterfaceId, Interface>,
    endpoints: BTreeMap<EndpointId, Endpoint>,
    components: BTreeMap<ComponentId, Component>,
}

impl Topology {
    pub(crate) fn new() -> Self {
        Self::default()
    }
    pub(crate) fn keyboards(&self) -> impl Iterator<Item = &LogicalKeyboard> {
        self.keyboards.values()
    }
    pub(crate) fn physical_devices(&self) -> impl Iterator<Item = &PhysicalDevice> {
        self.physical.values()
    }
    pub(crate) fn physical_device(&self, id: PhysicalDeviceId) -> Result<&PhysicalDevice> {
        self.physical
            .get(&id)
            .ok_or(ModelError::UnknownPhysicalDevice(id))
    }
    pub(crate) fn keyboard(&self, id: KeyboardId) -> Result<&LogicalKeyboard> {
        self.keyboards
            .get(&id)
            .ok_or(ModelError::UnknownKeyboard(id))
    }
    pub(crate) fn component(&self, id: ComponentId) -> Result<&Component> {
        self.components
            .get(&id)
            .ok_or(ModelError::UnknownComponent(id))
    }
    pub(crate) fn endpoint(&self, id: EndpointId) -> Result<&Endpoint> {
        self.endpoints
            .get(&id)
            .ok_or(ModelError::UnknownEndpoint(id))
    }
    pub(crate) fn interfaces(
        &self,
        physical: PhysicalDeviceId,
    ) -> impl Iterator<Item = &Interface> {
        self.interfaces
            .values()
            .filter(move |i| i.physical == physical)
    }
    pub(crate) fn endpoints(&self, physical: PhysicalDeviceId) -> impl Iterator<Item = &Endpoint> {
        self.endpoints
            .values()
            .filter(move |e| e.physical == physical)
    }
    pub(crate) fn components(
        &self,
        physical: PhysicalDeviceId,
    ) -> impl Iterator<Item = &Component> {
        self.components
            .values()
            .filter(move |c| c.physical == physical)
    }
    pub(crate) fn members(&self, keyboard: KeyboardId) -> impl Iterator<Item = &PhysicalDevice> {
        self.physical
            .values()
            .filter(move |p| p.keyboard == keyboard)
    }

    /// None starts a separate partial keyboard; aggregation is always explicit.
    pub(crate) fn add_physical_device(
        &mut self,
        keyboard: Option<KeyboardId>,
        spec: PhysicalDeviceSpec,
    ) -> Result<PhysicalDeviceId> {
        spec.validate()?;
        if let Some(id) = keyboard {
            self.keyboard(id)?;
        }
        let keyboard_id = match keyboard {
            Some(id) => id,
            None => KeyboardId::allocate()?,
        };
        let physical = PhysicalDeviceId::allocate()?;
        let membership = MembershipGeneration::allocate()?;
        let interface_ids: Vec<_> = spec
            .interfaces
            .iter()
            .map(|_| InterfaceId::allocate())
            .collect::<Result<_>>()?;
        let component_ids: Vec<_> = spec
            .components
            .iter()
            .map(|_| ComponentId::allocate())
            .collect::<Result<_>>()?;
        let mut endpoints = Vec::new();
        for endpoint in spec.endpoints {
            let id = EndpointId::allocate()?;
            endpoints.push(Endpoint {
                id,
                physical,
                interface: endpoint.interface.map(|index| interface_ids[index]),
                transport: endpoint.transport,
                locator: endpoint.locator,
                connection: Connection::new(),
                hid: endpoint.hid,
            });
        }
        // No fallible work follows: allocation exhaustion must not leave a
        // partially published physical device or an empty keyboard shell.
        self.capabilities.invalidate_keyboard(keyboard_id);
        if keyboard.is_none() {
            self.keyboards.insert(
                keyboard_id,
                LogicalKeyboard {
                    id: keyboard_id,
                    name: DisplayName::or_fallback(spec.name.clone(), "Unknown keyboard"),
                },
            );
        }
        self.physical.insert(
            physical,
            PhysicalDevice {
                id: physical,
                keyboard: keyboard_id,
                membership,
                name: DisplayName::or_fallback(spec.name, "Unknown physical device"),
                identity: spec.identity,
                usb_ids: spec.usb_ids,
                serial: spec.serial,
            },
        );
        for (id, interface) in interface_ids.into_iter().zip(spec.interfaces) {
            self.interfaces.insert(
                id,
                Interface {
                    id,
                    physical,
                    name: DisplayName::or_fallback(interface.name, "Interface"),
                    usb_number: interface.usb_number,
                },
            );
        }
        for (id, component) in component_ids.iter().copied().zip(spec.components) {
            self.components.insert(
                id,
                Component {
                    id,
                    physical,
                    keyboard: keyboard_id,
                    parent: component.parent.map(|index| component_ids[index]),
                    kind: component.kind,
                    name: DisplayName::or_fallback(component.name, component.kind.fallback()),
                },
            );
        }
        self.endpoints
            .extend(endpoints.into_iter().map(|e| (e.id, e)));
        Ok(physical)
    }

    /// The caller establishes association evidence; this operation never infers it
    /// from names, identifiers or the presence of left/right components.
    pub(crate) fn associate_physical_device(
        &mut self,
        physical: PhysicalDeviceId,
        keyboard: KeyboardId,
    ) -> Result<()> {
        self.keyboard(keyboard)?;
        let previous = self.physical_device(physical)?.keyboard;
        if previous == keyboard {
            return Ok(());
        }
        // Track reassociation even if a unit later returns to its old keyboard:
        // comparing only current keyboard IDs would revive stale operation tokens.
        let membership = MembershipGeneration::allocate()?;
        self.capabilities.invalidate_keyboard(previous);
        self.capabilities.invalidate_keyboard(keyboard);
        let device = self
            .physical
            .get_mut(&physical)
            .expect("validated physical device");
        device.keyboard = keyboard;
        device.membership = membership;
        for component in self
            .components
            .values_mut()
            .filter(|c| c.physical == physical)
        {
            component.keyboard = keyboard;
        }
        self.retire_empty_keyboard(previous);
        Ok(())
    }

    pub(crate) fn add_component(
        &mut self,
        physical: PhysicalDeviceId,
        parent: Option<ComponentId>,
        kind: ComponentKind,
        name: Option<DisplayName>,
    ) -> Result<ComponentId> {
        let keyboard = self.physical_device(physical)?.keyboard;
        if let Some(parent) = parent
            && self.component(parent)?.physical != physical
        {
            return Err(ModelError::CrossOwner {
                component: parent,
                physical,
            });
        }
        // Fresh IDs and an existing parent make cycles impossible on insertion.
        let id = ComponentId::allocate()?;
        self.capabilities.invalidate_keyboard(keyboard);
        self.components.insert(
            id,
            Component {
                id,
                physical,
                keyboard,
                parent,
                kind,
                name: DisplayName::or_fallback(name, kind.fallback()),
            },
        );
        Ok(id)
    }

    pub(crate) fn remove_component(&mut self, id: ComponentId) -> Result<()> {
        let keyboard = self.component(id)?.keyboard;
        let mut removed = BTreeSet::from([id]);
        loop {
            let descendants: Vec<_> = self
                .components
                .values()
                .filter(|c| c.parent.is_some_and(|parent| removed.contains(&parent)))
                .map(|c| c.id)
                .collect();
            let before = removed.len();
            removed.extend(descendants);
            if before == removed.len() {
                break;
            }
        }
        self.capabilities.invalidate_keyboard(keyboard);
        self.components.retain(|id, _| !removed.contains(id));
        Ok(())
    }

    pub(crate) fn retire_physical_device(&mut self, physical: PhysicalDeviceId) -> Result<()> {
        let keyboard = self.physical_device(physical)?.keyboard;
        self.capabilities.invalidate_keyboard(keyboard);
        self.components.retain(|_, c| c.physical != physical);
        self.endpoints.retain(|_, e| e.physical != physical);
        self.interfaces.retain(|_, i| i.physical != physical);
        self.physical.remove(&physical);
        self.retire_empty_keyboard(keyboard);
        Ok(())
    }

    fn retire_empty_keyboard(&mut self, keyboard: KeyboardId) {
        if !self.physical.values().any(|p| p.keyboard == keyboard) {
            self.keyboards.remove(&keyboard);
        }
    }

    pub(crate) fn rename_keyboard(&mut self, id: KeyboardId, name: DisplayName) -> Result<()> {
        self.keyboards
            .get_mut(&id)
            .ok_or(ModelError::UnknownKeyboard(id))?
            .name = name;
        Ok(())
    }

    pub(crate) fn rename_physical_device(
        &mut self,
        id: PhysicalDeviceId,
        name: DisplayName,
    ) -> Result<()> {
        self.physical
            .get_mut(&id)
            .ok_or(ModelError::UnknownPhysicalDevice(id))?
            .name = name;
        Ok(())
    }

    pub(crate) fn identity_candidates(&self, evidence: &HardwareIdentity) -> IdentityCandidates {
        if !evidence.has_verified_unit_claim() {
            return IdentityCandidates::InsufficientEvidence;
        }
        let candidates: Vec<_> = self
            .physical
            .values()
            .filter(|p| p.identity.compare(evidence) == IdentityComparison::Matching)
            .map(|p| p.id)
            .collect();
        match candidates.as_slice() {
            [] => IdentityCandidates::NoMatch,
            [one] => IdentityCandidates::One(*one),
            _ => IdentityCandidates::Ambiguous(candidates),
        }
    }

    pub(crate) fn revalidate_endpoint(&mut self, id: EndpointId) -> Result<AttachmentGeneration> {
        let keyboard = self.physical_device(self.endpoint(id)?.physical)?.keyboard;
        let generation = self
            .endpoints
            .get_mut(&id)
            .ok_or(ModelError::UnknownEndpoint(id))?
            .connection
            .revalidate()?;
        self.capabilities.invalidate_keyboard(keyboard);
        Ok(generation)
    }

    pub(crate) fn invalidate_endpoint(
        &mut self,
        id: EndpointId,
        reason: UnavailableReason,
    ) -> Result<()> {
        let keyboard = self.physical_device(self.endpoint(id)?.physical)?.keyboard;
        self.endpoints
            .get_mut(&id)
            .ok_or(ModelError::UnknownEndpoint(id))?
            .connection
            .invalidate(reason);
        match reason {
            // Access failure alone is not evidence that supported operations have
            // changed. Revalidation will invalidate this retained context later.
            UnavailableReason::PermissionDenied | UnavailableReason::Busy => {
                self.capabilities.cancel_pending_from(id)
            }
            UnavailableReason::Disconnected
            | UnavailableReason::Suspended
            | UnavailableReason::ProtocolFailure => self.capabilities.invalidate_keyboard(keyboard),
        }
        Ok(())
    }

    pub(crate) fn begin_operation(
        &self,
        endpoint: EndpointId,
        component: ComponentId,
    ) -> Result<OperationToken> {
        let channel = self.endpoint(endpoint)?;
        let target = self.component(component)?;
        let source = self.physical_device(channel.physical)?;
        let keyboard = source.keyboard;
        if target.keyboard != keyboard {
            return Err(ModelError::CrossKeyboard {
                endpoint,
                component,
            });
        }
        let ConnectionState::Ready(generation) = channel.state() else {
            return Err(ModelError::EndpointNotReady(endpoint));
        };
        Ok(OperationToken {
            endpoint,
            generation,
            component,
            keyboard,
            endpoint_membership: source.membership,
            target_membership: self.physical_device(target.physical)?.membership,
        })
    }

    pub(crate) fn begin_attachment(&self, endpoint: EndpointId) -> Result<AttachmentToken> {
        let channel = self.endpoint(endpoint)?;
        let source = self.physical_device(channel.physical)?;
        let ConnectionState::Ready(generation) = channel.state() else {
            return Err(ModelError::EndpointNotReady(endpoint));
        };
        Ok(AttachmentToken {
            endpoint,
            generation,
            physical: source.id,
            keyboard: source.keyboard,
            membership: source.membership,
        })
    }

    pub(crate) fn validate_attachment(&self, token: AttachmentToken) -> Result<()> {
        if self
            .begin_attachment(token.endpoint)
            .is_ok_and(|current| current == token)
        {
            Ok(())
        } else {
            Err(ModelError::StaleAttachment(token.endpoint))
        }
    }

    pub(crate) fn validate_completion(&self, token: OperationToken) -> Result<()> {
        // Also remember original membership: moving both endpoint and target to
        // another keyboard must not make work issued for the old keyboard current.
        if self
            .begin_operation(token.endpoint, token.component)
            .is_ok_and(|current| current == token)
        {
            Ok(())
        } else {
            Err(ModelError::StaleOperation {
                endpoint: token.endpoint,
                component: token.component,
            })
        }
    }
}
