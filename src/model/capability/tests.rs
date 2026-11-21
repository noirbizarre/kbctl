use super::*;
use crate::model::{
    connection::{EndpointLocator, TransportKind},
    identity::{HardwareIdentity, IdentityAbsence, PhysicalDeviceId},
    topology::{ComponentKind, ComponentSpec, DisplayName, EndpointSpec, PhysicalDeviceSpec},
};

use CapabilityOperation as Op;

fn unit() -> PhysicalDeviceSpec {
    PhysicalDeviceSpec {
        name: None,
        identity: HardwareIdentity::missing(IdentityAbsence::NotReported),
        usb_ids: None,
        serial: None,
        interfaces: vec![],
        endpoints: vec![EndpointSpec {
            interface: None,
            transport: TransportKind::Serial,
            locator: EndpointLocator::new("synthetic-channel").unwrap(),
            hid: Default::default(),
        }],
        components: vec![ComponentSpec {
            name: None,
            kind: ComponentKind::Body,
            parent: None,
        }],
    }
}

struct Fixture {
    topology: Topology,
    physical: PhysicalDeviceId,
    keyboard: KeyboardId,
    component: ComponentId,
    source: EndpointId,
}

impl Fixture {
    fn new() -> Self {
        let mut topology = Topology::new();
        let physical = topology.add_physical_device(None, unit()).unwrap();
        let keyboard = topology.physical_device(physical).unwrap().keyboard();
        let component = topology.components(physical).next().unwrap().id();
        let source = topology.endpoints(physical).next().unwrap().id();
        topology.revalidate_endpoint(source).unwrap();
        Self {
            topology,
            physical,
            keyboard,
            component,
            source,
        }
    }
    fn scope(&self) -> CapabilityScope {
        CapabilityScope::Component(self.component)
    }
    fn keyboard_scope(&self) -> CapabilityScope {
        CapabilityScope::Keyboard(self.keyboard)
    }
}

fn report(entries: &[(CapabilityOperation, Support)]) -> CapabilityReport {
    CapabilityReport::new(
        "synthetic.reporter",
        vec![],
        entries
            .iter()
            .map(|(op, support)| (*op, CapabilityEntry::new(*support, vec![])))
            .collect(),
    )
    .unwrap()
}

fn publish(
    topology: &mut Topology,
    scope: CapabilityScope,
    source: EndpointId,
    entries: &[(CapabilityOperation, Support)],
) -> CapabilityRefreshToken {
    let token = topology.begin_capability_refresh(scope, source).unwrap();
    topology
        .complete_capability_refresh(token, report(entries))
        .unwrap();
    token
}

fn assert_unknown(topology: &Topology, scope: CapabilityScope) {
    let view = topology.capabilities(scope).unwrap();
    assert_eq!(view.discovery(), CapabilityDiscovery::NotDiscovered);
    assert_eq!(view.support(Op::ReadBattery), Support::Unknown);
    assert!(view.report().is_none());
}

#[test]
fn partial_reports_distinguish_unprobed_writes_from_explicitly_unsupported_operations() {
    let mut f = Fixture::new();
    let scope = f.scope();
    assert_unknown(&f.topology, scope);
    let token = publish(
        &mut f.topology,
        scope,
        f.source,
        &[
            (Op::ReadLayers, Support::Supported),
            (Op::WriteMacros, Support::Unsupported),
        ],
    );
    let view = f.topology.capabilities(scope).unwrap();
    assert_eq!(view.support(Op::ReadLayers), Support::Supported);
    assert_eq!(view.support(Op::ActivateLayer), Support::Unknown);
    assert_eq!(view.support(Op::WriteMacros), Support::Unsupported);
    assert_eq!(view.support(Op::ReadBattery), Support::Unknown);
    assert_eq!(view.report().unwrap().operations().count(), 2);
    assert_eq!(view.provenance(), Some(token));
    assert_eq!(token.source(), f.source);
    assert_eq!(token.scope(), scope);
    assert_eq!(view.report().unwrap().reporter(), "synthetic.reporter");
    assert_eq!(view.source_availability(), SourceAvailability::Available);
}

#[test]
fn operation_access_describes_semantic_writes_not_transport_or_subscription_bookkeeping() {
    for read in [
        Op::ReadIndicators,
        Op::ReadLayers,
        Op::ReadLighting,
        Op::ReadBattery,
        Op::ReadWirelessState,
        Op::ReadKeymaps,
        Op::ReadMacros,
        Op::SubscribeRuntimeEvents,
        Op::ReadFirmware,
    ] {
        assert_eq!(read.access(), OperationAccess::ReadOnly);
    }
    for write in [
        Op::WriteIndicators,
        Op::ActivateLayer,
        Op::WriteLighting,
        Op::WriteKeymaps,
        Op::WriteMacros,
    ] {
        assert_eq!(write.access(), OperationAccess::Writable);
    }
}

#[test]
fn component_reports_do_not_inherit_or_union_into_keyboard_or_sibling_support() {
    let mut f = Fixture::new();
    let other = f
        .topology
        .add_physical_device(Some(f.keyboard), unit())
        .unwrap();
    let remote = f.topology.components(other).next().unwrap().id();
    let remote_scope = CapabilityScope::Component(remote);
    publish(
        &mut f.topology,
        remote_scope,
        f.source,
        &[(Op::ReadBattery, Support::Supported)],
    );
    assert_unknown(&f.topology, f.scope());
    assert_unknown(&f.topology, f.keyboard_scope());
    let keyboard = f.keyboard_scope();
    publish(
        &mut f.topology,
        keyboard,
        f.source,
        &[(Op::ReadIndicators, Support::Supported)],
    );
    assert_eq!(
        f.topology
            .capabilities(remote_scope)
            .unwrap()
            .support(Op::ReadBattery),
        Support::Supported
    );
    assert_eq!(
        f.topology
            .capabilities(remote_scope)
            .unwrap()
            .support(Op::ReadIndicators),
        Support::Unknown
    );
    assert_eq!(
        f.topology
            .capabilities(keyboard)
            .unwrap()
            .support(Op::ReadBattery),
        Support::Unknown
    );
}

#[test]
fn keyboard_discovery_does_not_need_a_representative_component() {
    let mut topology = Topology::new();
    let mut spec = unit();
    spec.components.clear();
    let physical = topology.add_physical_device(None, spec).unwrap();
    let keyboard = topology.physical_device(physical).unwrap().keyboard();
    let source = topology.endpoints(physical).next().unwrap().id();
    topology.revalidate_endpoint(source).unwrap();
    publish(
        &mut topology,
        CapabilityScope::Keyboard(keyboard),
        source,
        &[(Op::ReadFirmware, Support::Supported)],
    );
    assert_eq!(
        topology
            .capabilities(CapabilityScope::Keyboard(keyboard))
            .unwrap()
            .support(Op::ReadFirmware),
        Support::Supported
    );
}

/// Synthetic reporter policy: only this reporter understands its release names.
fn firmware_report(firmware: Option<&str>) -> CapabilityReport {
    let support = match firmware {
        Some("release-blue") => Support::Supported,
        Some("legacy-r4") => Support::Unsupported,
        _ => Support::Unknown,
    };
    let mut versions =
        vec![ReportedVersion::new(VersionKind::Protocol, "synthetic/wire", "dialect-B").unwrap()];
    if let Some(value) = firmware {
        versions.push(
            ReportedVersion::new(VersionKind::Firmware, "synthetic/firmware", value).unwrap(),
        );
    }
    CapabilityReport::new(
        "synthetic.reporter",
        versions,
        vec![
            (
                Op::ReadLayers,
                CapabilityEntry::new(Support::Supported, vec![]),
            ),
            (
                Op::ActivateLayer,
                CapabilityEntry::new(
                    support,
                    vec![
                        CapabilityConstraint::new(
                            "synthetic/release-policy",
                            "firmware belongs to blue releases; protocol is dialect-B",
                        )
                        .unwrap(),
                    ],
                ),
            ),
        ],
    )
    .unwrap()
}

#[test]
fn firmware_dependent_support_is_resolved_by_the_reporter_without_universal_version_ordering() {
    let mut f = Fixture::new();
    let scope = f.scope();
    for (firmware, expected) in [
        (Some("legacy-r4"), Support::Unsupported),
        (Some("release-blue"), Support::Supported),
        (None, Support::Unknown),
        (Some("unrecognized-preview"), Support::Unknown),
    ] {
        let token = f
            .topology
            .begin_capability_refresh(scope, f.source)
            .unwrap();
        f.topology
            .complete_capability_refresh(token, firmware_report(firmware))
            .unwrap();
        let view = f.topology.capabilities(scope).unwrap();
        assert_eq!(view.support(Op::ReadLayers), Support::Supported);
        assert_eq!(view.support(Op::ActivateLayer), expected);
        let report = view.report().unwrap();
        assert_eq!(
            report
                .versions()
                .iter()
                .find(|v| v.kind() == VersionKind::Firmware)
                .map(|v| v.value()),
            firmware
        );
        let protocol = &report.versions()[0];
        assert_eq!(protocol.namespace(), "synthetic/wire");
        assert_eq!(protocol.value(), "dialect-B");
        let constraint = &report.entry(Op::ActivateLayer).unwrap().constraints()[0];
        assert_eq!(constraint.namespace(), "synthetic/release-policy");
        assert_eq!(
            constraint.expression(),
            "firmware belongs to blue releases; protocol is dialect-B"
        );
    }
}

#[test]
fn refresh_replaces_the_whole_scope_and_does_not_retain_omitted_support() {
    let mut f = Fixture::new();
    let scope = f.scope();
    publish(
        &mut f.topology,
        scope,
        f.source,
        &[(Op::ReadBattery, Support::Supported)],
    );
    let token = f
        .topology
        .begin_capability_refresh(scope, f.source)
        .unwrap();
    let view = f.topology.capabilities(scope).unwrap();
    assert_eq!(view.discovery(), CapabilityDiscovery::Refreshing);
    assert_eq!(view.support(Op::ReadBattery), Support::Unknown);
    assert!(view.report().is_none());
    f.topology
        .complete_capability_refresh(token, report(&[(Op::ReadLighting, Support::Supported)]))
        .unwrap();
    let view = f.topology.capabilities(scope).unwrap();
    assert_eq!(view.support(Op::ReadLighting), Support::Supported);
    assert_eq!(view.support(Op::ReadBattery), Support::Unknown);
    let token = f
        .topology
        .begin_capability_refresh(scope, f.source)
        .unwrap();
    f.topology
        .complete_capability_refresh(token, report(&[]))
        .unwrap();
    let view = f.topology.capabilities(scope).unwrap();
    assert_eq!(view.discovery(), CapabilityDiscovery::Reported);
    assert_eq!(view.support(Op::ReadLighting), Support::Unknown);
    assert!(view.report().unwrap().versions().is_empty());
}

#[test]
fn the_latest_refresh_start_wins_even_on_the_same_attachment() {
    let mut f = Fixture::new();
    let scope = f.scope();
    let old = f
        .topology
        .begin_capability_refresh(scope, f.source)
        .unwrap();
    let new = f
        .topology
        .begin_capability_refresh(scope, f.source)
        .unwrap();
    assert_ne!(old.id(), new.id());
    assert_eq!(old.attachment(), new.attachment());
    assert!(matches!(
        f.topology
            .complete_capability_refresh(old, report(&[(Op::ReadBattery, Support::Unsupported)])),
        Err(ModelError::StaleCapabilityRefresh { .. })
    ));
    assert!(f.topology.abandon_capability_refresh(old).is_err());
    assert_eq!(
        f.topology.capabilities(scope).unwrap().discovery(),
        CapabilityDiscovery::Refreshing
    );
    f.topology
        .complete_capability_refresh(new, report(&[(Op::ReadBattery, Support::Supported)]))
        .unwrap();
    for stale in [old, new] {
        assert!(
            f.topology
                .complete_capability_refresh(stale, report(&[]))
                .is_err()
        );
        assert!(f.topology.abandon_capability_refresh(stale).is_err());
        assert_eq!(
            f.topology
                .capabilities(scope)
                .unwrap()
                .support(Op::ReadBattery),
            Support::Supported
        );
    }
}

#[test]
fn switching_report_sources_does_not_merge_their_results() {
    let mut f = Fixture::new();
    let physical = f
        .topology
        .add_physical_device(Some(f.keyboard), unit())
        .unwrap();
    let source = f.topology.endpoints(physical).next().unwrap().id();
    f.topology.revalidate_endpoint(source).unwrap();
    let scope = f.scope();
    let old = f
        .topology
        .begin_capability_refresh(scope, f.source)
        .unwrap();
    let current = f.topology.begin_capability_refresh(scope, source).unwrap();
    f.topology
        .complete_capability_refresh(current, report(&[(Op::ReadFirmware, Support::Supported)]))
        .unwrap();
    assert!(
        f.topology
            .complete_capability_refresh(old, report(&[(Op::ReadBattery, Support::Supported)]))
            .is_err()
    );
    let view = f.topology.capabilities(scope).unwrap();
    assert_eq!(view.provenance().unwrap().source(), source);
    assert_eq!(view.support(Op::ReadBattery), Support::Unknown);
}

#[test]
fn abandoning_current_work_leaves_support_unknown() {
    let mut f = Fixture::new();
    let scope = f.scope();
    let token = f
        .topology
        .begin_capability_refresh(scope, f.source)
        .unwrap();
    f.topology.abandon_capability_refresh(token).unwrap();
    assert_unknown(&f.topology, scope);
    assert!(
        f.topology
            .complete_capability_refresh(token, report(&[]))
            .is_err()
    );
}

#[test]
fn rejected_refresh_sources_preserve_previously_accepted_evidence() {
    let mut f = Fixture::new();
    let sibling = f
        .topology
        .add_physical_device(Some(f.keyboard), unit())
        .unwrap();
    let unready = f.topology.endpoints(sibling).next().unwrap().id();
    let foreign = f.topology.add_physical_device(None, unit()).unwrap();
    let foreign_source = f.topology.endpoints(foreign).next().unwrap().id();
    let retired = f.topology.components(sibling).next().unwrap().id();
    f.topology.remove_component(retired).unwrap();
    let scope = f.scope();
    let provenance = publish(
        &mut f.topology,
        scope,
        f.source,
        &[(Op::ReadBattery, Support::Supported)],
    );
    assert!(matches!(
        f.topology.begin_capability_refresh(scope, foreign_source),
        Err(ModelError::CapabilitySourceMismatch { .. })
    ));
    assert_eq!(
        f.topology.begin_capability_refresh(scope, unready),
        Err(ModelError::EndpointNotReady(unready))
    );
    assert_eq!(
        f.topology
            .begin_capability_refresh(CapabilityScope::Component(retired), f.source),
        Err(ModelError::UnknownComponent(retired))
    );
    assert_eq!(
        f.topology.capabilities(scope).unwrap().provenance(),
        Some(provenance)
    );
}

#[test]
fn malformed_reports_cannot_replace_a_pending_refresh() {
    let mut f = Fixture::new();
    let scope = f.scope();
    let token = f
        .topology
        .begin_capability_refresh(scope, f.source)
        .unwrap();
    assert!(CapabilityReport::new(" ", vec![], vec![]).is_err());
    assert_eq!(
        CapabilityReport::new(
            "reporter",
            vec![],
            vec![
                (
                    Op::ReadBattery,
                    CapabilityEntry::new(Support::Supported, vec![])
                ),
                (
                    Op::ReadBattery,
                    CapabilityEntry::new(Support::Unsupported, vec![])
                )
            ]
        ),
        Err(ModelError::DuplicateCapability(Op::ReadBattery))
    );
    let version = ReportedVersion::new(VersionKind::Firmware, "issuer", "v1").unwrap();
    assert!(matches!(
        CapabilityReport::new("reporter", vec![version.clone(), version], vec![]),
        Err(ModelError::DuplicateCapabilityVersion { .. })
    ));
    for (namespace, value) in [("", "value"), ("namespace", "\n")] {
        assert!(ReportedVersion::new(VersionKind::Protocol, namespace, value).is_err());
        assert!(CapabilityConstraint::new(namespace, value).is_err());
    }
    assert_eq!(
        f.topology.capabilities(scope).unwrap().discovery(),
        CapabilityDiscovery::Refreshing
    );
    f.topology
        .complete_capability_refresh(token, report(&[]))
        .unwrap();
    let exact = ReportedVersion::new(VersionKind::Firmware, "issuer", " release-blue ").unwrap();
    assert_eq!(exact.value(), " release-blue ");
}

#[test]
fn structural_changes_invalidate_reports_and_pending_refreshes_for_the_whole_keyboard() {
    for change in 0..5 {
        let mut f = Fixture::new();
        let member = f
            .topology
            .add_physical_device(Some(f.keyboard), unit())
            .unwrap();
        let destination = f.topology.add_physical_device(None, unit()).unwrap();
        let destination_keyboard = f.topology.physical_device(destination).unwrap().keyboard();
        let module = f
            .topology
            .add_component(f.physical, Some(f.component), ComponentKind::Module, None)
            .unwrap();
        let scope = f.scope();
        let keyboard_scope = f.keyboard_scope();
        publish(
            &mut f.topology,
            keyboard_scope,
            f.source,
            &[(Op::ReadBattery, Support::Supported)],
        );
        let pending = f
            .topology
            .begin_capability_refresh(scope, f.source)
            .unwrap();
        match change {
            0 => {
                f.topology
                    .add_physical_device(Some(f.keyboard), unit())
                    .unwrap();
            }
            1 => {
                f.topology
                    .add_component(f.physical, None, ComponentKind::Receiver, None)
                    .unwrap();
            }
            2 => f.topology.remove_component(module).unwrap(),
            3 => f.topology.retire_physical_device(member).unwrap(),
            _ => {
                f.topology
                    .associate_physical_device(member, destination_keyboard)
                    .unwrap();
                f.topology
                    .associate_physical_device(member, f.keyboard)
                    .unwrap();
            }
        }
        assert_unknown(&f.topology, keyboard_scope);
        assert_unknown(&f.topology, scope);
        assert!(
            f.topology
                .complete_capability_refresh(pending, report(&[]))
                .is_err()
        );
    }
}

#[test]
fn target_retirement_and_rediscovery_never_revive_cached_support_or_pending_work() {
    let mut f = Fixture::new();
    let scope = f.scope();
    let keyboard_scope = f.keyboard_scope();
    publish(
        &mut f.topology,
        scope,
        f.source,
        &[(Op::ReadBattery, Support::Supported)],
    );
    let pending = f
        .topology
        .begin_capability_refresh(keyboard_scope, f.source)
        .unwrap();
    f.topology.remove_component(f.component).unwrap();
    assert!(matches!(
        f.topology.capabilities(scope),
        Err(ModelError::UnknownComponent(_))
    ));
    assert_unknown(&f.topology, keyboard_scope);
    assert!(
        f.topology
            .complete_capability_refresh(pending, report(&[]))
            .is_err()
    );
    let replacement = f
        .topology
        .add_component(f.physical, None, ComponentKind::Body, None)
        .unwrap();
    assert_unknown(&f.topology, CapabilityScope::Component(replacement));
    let pending = f
        .topology
        .begin_capability_refresh(CapabilityScope::Component(replacement), f.source)
        .unwrap();
    f.topology.retire_physical_device(f.physical).unwrap();
    assert!(
        f.topology
            .complete_capability_refresh(pending, report(&[]))
            .is_err()
    );
    assert!(matches!(
        f.topology.capabilities(keyboard_scope),
        Err(ModelError::UnknownKeyboard(_))
    ));
    let rediscovered = f.topology.add_physical_device(None, unit()).unwrap();
    let component = f.topology.components(rediscovered).next().unwrap().id();
    assert_unknown(&f.topology, CapabilityScope::Component(component));
}

#[test]
fn disconnect_suspend_and_protocol_failure_clear_support_and_reject_late_discovery() {
    for reason in [
        UnavailableReason::Disconnected,
        UnavailableReason::Suspended,
        UnavailableReason::ProtocolFailure,
    ] {
        let mut f = Fixture::new();
        let scope = f.scope();
        let keyboard_scope = f.keyboard_scope();
        publish(
            &mut f.topology,
            scope,
            f.source,
            &[(Op::ReadBattery, Support::Supported)],
        );
        let pending = f
            .topology
            .begin_capability_refresh(keyboard_scope, f.source)
            .unwrap();
        f.topology.invalidate_endpoint(f.source, reason).unwrap();
        assert_unknown(&f.topology, scope);
        assert_unknown(&f.topology, keyboard_scope);
        assert!(
            f.topology
                .complete_capability_refresh(pending, report(&[]))
                .is_err()
        );
    }
}

#[test]
fn temporary_access_loss_preserves_established_support_but_cancels_pending_discovery() {
    for reason in [UnavailableReason::PermissionDenied, UnavailableReason::Busy] {
        let mut f = Fixture::new();
        let scope = f.scope();
        let keyboard_scope = f.keyboard_scope();
        let provenance = publish(
            &mut f.topology,
            scope,
            f.source,
            &[(Op::ReadBattery, Support::Supported)],
        );
        let pending = f
            .topology
            .begin_capability_refresh(keyboard_scope, f.source)
            .unwrap();
        f.topology.invalidate_endpoint(f.source, reason).unwrap();
        let view = f.topology.capabilities(scope).unwrap();
        assert_eq!(view.support(Op::ReadBattery), Support::Supported);
        assert_eq!(
            view.source_availability(),
            SourceAvailability::Unavailable(reason)
        );
        assert_eq!(view.provenance(), Some(provenance));
        assert_unknown(&f.topology, keyboard_scope);
        assert!(
            f.topology
                .complete_capability_refresh(pending, report(&[]))
                .is_err()
        );
        assert_eq!(
            f.topology.begin_capability_refresh(scope, f.source),
            Err(ModelError::EndpointNotReady(f.source))
        );
        f.topology.revalidate_endpoint(f.source).unwrap();
        assert_unknown(&f.topology, scope);
        assert!(
            f.topology
                .complete_capability_refresh(pending, report(&[]))
                .is_err()
        );
    }
}

#[test]
fn revalidation_and_explicit_context_changes_invalidate_evidence_without_an_unplug() {
    for revalidate in [false, true] {
        let mut f = Fixture::new();
        let scope = f.scope();
        let keyboard_scope = f.keyboard_scope();
        publish(
            &mut f.topology,
            scope,
            f.source,
            &[(Op::ReadBattery, Support::Supported)],
        );
        let pending = f
            .topology
            .begin_capability_refresh(keyboard_scope, f.source)
            .unwrap();
        if revalidate {
            f.topology.revalidate_endpoint(f.source).unwrap();
        } else {
            f.topology.invalidate_capabilities(f.keyboard).unwrap();
        }
        assert_unknown(&f.topology, scope);
        assert_unknown(&f.topology, keyboard_scope);
        assert!(
            f.topology
                .complete_capability_refresh(pending, report(&[]))
                .is_err()
        );
        let fresh = f
            .topology
            .begin_capability_refresh(keyboard_scope, f.source)
            .unwrap();
        assert_ne!(fresh.id(), pending.id());
    }
}

#[test]
fn renames_no_op_association_and_failed_edits_preserve_reports_and_pending_work() {
    let mut f = Fixture::new();
    let scope = f.scope();
    let keyboard_scope = f.keyboard_scope();
    let provenance = publish(
        &mut f.topology,
        scope,
        f.source,
        &[(Op::ReadBattery, Support::Supported)],
    );
    let pending = f
        .topology
        .begin_capability_refresh(keyboard_scope, f.source)
        .unwrap();
    f.topology
        .rename_keyboard(f.keyboard, DisplayName::new("desk").unwrap())
        .unwrap();
    f.topology
        .rename_physical_device(f.physical, DisplayName::new("controller").unwrap())
        .unwrap();
    f.topology
        .associate_physical_device(f.physical, f.keyboard)
        .unwrap();
    let mut invalid = unit();
    invalid.components[0].parent = Some(0);
    assert!(
        f.topology
            .add_physical_device(Some(f.keyboard), invalid)
            .is_err()
    );
    assert_eq!(
        f.topology.capabilities(scope).unwrap().provenance(),
        Some(provenance)
    );
    f.topology
        .complete_capability_refresh(pending, report(&[]))
        .unwrap();
}

#[test]
fn another_keyboard_and_an_unaffected_pending_source_keep_their_own_evidence() {
    let mut f = Fixture::new();
    let remote = f
        .topology
        .add_physical_device(Some(f.keyboard), unit())
        .unwrap();
    let remote_source = f.topology.endpoints(remote).next().unwrap().id();
    f.topology.revalidate_endpoint(remote_source).unwrap();
    let unrelated = f.topology.add_physical_device(None, unit()).unwrap();
    let unrelated_keyboard = f.topology.physical_device(unrelated).unwrap().keyboard();
    let unrelated_source = f.topology.endpoints(unrelated).next().unwrap().id();
    f.topology.revalidate_endpoint(unrelated_source).unwrap();
    let scope = f.scope();
    publish(
        &mut f.topology,
        scope,
        f.source,
        &[(Op::ReadBattery, Support::Supported)],
    );
    let unrelated_scope = CapabilityScope::Keyboard(unrelated_keyboard);
    let unrelated_token = publish(
        &mut f.topology,
        unrelated_scope,
        unrelated_source,
        &[(Op::ReadFirmware, Support::Supported)],
    );
    let pending = f
        .topology
        .begin_capability_refresh(f.keyboard_scope(), remote_source)
        .unwrap();
    f.topology
        .invalidate_endpoint(f.source, UnavailableReason::Busy)
        .unwrap();
    f.topology
        .complete_capability_refresh(pending, report(&[]))
        .unwrap();
    f.topology.invalidate_capabilities(f.keyboard).unwrap();
    assert_eq!(
        f.topology
            .capabilities(unrelated_scope)
            .unwrap()
            .provenance(),
        Some(unrelated_token)
    );
    f.topology
        .add_component(unrelated, None, ComponentKind::Module, None)
        .unwrap();
    assert_unknown(&f.topology, unrelated_scope);
}
