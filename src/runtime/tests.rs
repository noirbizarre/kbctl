use super::*;
use crate::model::{
    identity::{HardwareIdentity, IdentityAbsence},
    observation::{HostLock, Percentage},
    topology::{ComponentSpec, EndpointSpec, InterfaceSpec},
};
use crate::test_support::clock::ManualClock as Manual;
use std::time::Duration;
struct Epochs {
    next: u8,
    fail_at: Option<u8>,
}
impl EpochSource for Epochs {
    fn next(&mut self) -> std::result::Result<Epoch, StateError> {
        if self.fail_at == Some(self.next) {
            return Err(StateError::Epoch(std::io::Error::other(
                "synthetic failure",
            )));
        }
        let epoch = Epoch::from_bytes([self.next; 16]);
        self.next += 1;
        Ok(epoch)
    }
}
fn setup() -> (Runtime, Arc<Manual>) {
    let clock = Arc::new(Manual::new());
    let runtime = Runtime::with_epochs(
        Topology::new(),
        clock.clone(),
        Box::new(Epochs {
            next: 1,
            fail_at: None,
        }),
    )
    .unwrap();
    (runtime, clock)
}
fn scope() -> NamespacedId {
    NamespacedId::new("seat", "explicit-scope").unwrap()
}
fn patch(lock: HostLock, value: HostValue) -> HostPatch {
    HostPatch(BTreeMap::from([(lock, value)]))
}
fn host(runtime: &Runtime, handle: HostHandle) -> HostState {
    let snapshot = runtime.reader().snapshot().unwrap();
    let StateRecord::Host(host) = snapshot.records[&RecordKey::Host(handle.id())].as_ref() else {
        panic!("host record expected");
    };
    host.as_ref().clone()
}
fn unit() -> PhysicalDeviceSpec {
    PhysicalDeviceSpec {
        name: None,
        identity: HardwareIdentity::missing(IdentityAbsence::NotReported),
        usb_ids: None,
        serial: None,
        interfaces: vec![],
        endpoints: vec![],
        components: vec![],
    }
}

#[test]
fn absent_host_patch_fields_preserve_their_original_values_stamps_and_deadlines() {
    let (mut runtime, clock) = setup();
    let (handle, _) = runtime.register_host(scope(), clock.at(0)).unwrap();
    runtime
        .apply_host(
            handle,
            patch(HostLock::Caps, HostValue::Known(false)),
            1,
            clock.time(0),
            clock.time(10),
            clock.at(0),
        )
        .unwrap();
    let old = host(&runtime, handle).fields[&HostLock::Caps].clone();
    runtime
        .apply_host(
            handle,
            patch(HostLock::Num, HostValue::Known(true)),
            2,
            clock.time(1),
            clock.time(20),
            clock.at(1),
        )
        .unwrap();
    let state = host(&runtime, handle);
    assert_eq!(state.fields[&HostLock::Caps], old);
    assert!(!state.fields.contains_key(&HostLock::Scroll));
    assert_eq!(
        state.fields[&HostLock::Num]
            .report
            .as_ref()
            .unwrap()
            .stamp
            .fresh_until,
        Timestamp(Duration::from_secs(20))
    );
}

#[test]
fn explicit_unknown_withdraws_current_confidence_without_rewriting_last_known_provenance() {
    let (mut runtime, clock) = setup();
    let (handle, _) = runtime.register_host(scope(), clock.at(0)).unwrap();
    runtime
        .apply_host(
            handle,
            patch(HostLock::Caps, HostValue::Known(true)),
            1,
            clock.time(0),
            clock.time(10),
            clock.at(0),
        )
        .unwrap();
    let old = host(&runtime, handle).fields[&HostLock::Caps]
        .last_known
        .clone();
    runtime
        .apply_host(
            handle,
            patch(HostLock::Caps, HostValue::Unknown),
            2,
            clock.time(1),
            clock.time(10),
            clock.at(1),
        )
        .unwrap();
    let state = host(&runtime, handle);
    let field = &state.fields[&HostLock::Caps];
    assert_eq!(field.report.as_ref().unwrap().value, Observation::Unknown);
    assert_eq!(field.last_known, old);
    assert_eq!(field.last_known.as_ref().unwrap().stamp.sequence, 1);
}

#[test]
fn scopes_and_sources_do_not_elect_a_host_lock_winner_or_copy_into_device_state() {
    let (mut runtime, clock) = setup();
    let (a, _) = runtime.register_host(scope(), clock.at(0)).unwrap();
    let (b, _) = runtime.register_host(scope(), clock.at(0)).unwrap();
    runtime
        .apply_host(
            a,
            patch(HostLock::Caps, HostValue::Known(true)),
            1,
            clock.time(0),
            clock.time(10),
            clock.at(0),
        )
        .unwrap();
    runtime
        .apply_host(
            b,
            patch(HostLock::Caps, HostValue::Known(false)),
            1,
            clock.time(0),
            clock.time(10),
            clock.at(0),
        )
        .unwrap();
    assert_eq!(
        host(&runtime, a).fields[&HostLock::Caps]
            .report
            .as_ref()
            .unwrap()
            .value,
        Observation::Known(true)
    );
    assert_eq!(
        host(&runtime, b).fields[&HostLock::Caps]
            .report
            .as_ref()
            .unwrap()
            .value,
        Observation::Known(false)
    );
    assert!(
        runtime
            .reader()
            .snapshot()
            .unwrap()
            .records
            .keys()
            .all(|key| !matches!(key, RecordKey::Device(..)))
    );
}

#[test]
fn unavailable_host_collection_requires_a_fresh_generation_instead_of_reporting_false() {
    for reason in [
        HostUnavailable::Unsupported,
        HostUnavailable::PermissionDenied,
        HostUnavailable::Inaccessible,
        HostUnavailable::EventGap,
    ] {
        let (mut runtime, clock) = setup();
        let (handle, _) = runtime.register_host(scope(), clock.at(0)).unwrap();
        runtime
            .apply_host(
                handle,
                patch(HostLock::Caps, HostValue::Known(true)),
                3,
                clock.time(0),
                clock.time(10),
                clock.at(0),
            )
            .unwrap();
        runtime
            .host_unavailable(handle, reason, clock.at(1))
            .unwrap();
        let state = host(&runtime, handle);
        let field = &state.fields[&HostLock::Caps];
        assert_eq!(
            state.availability,
            SourceAvailability::Unavailable(reason.failure())
        );
        assert_eq!(field.quality(), FieldQuality::Stale);
        assert_eq!(
            field.report.as_ref().unwrap().value,
            Observation::Known(true)
        );
        assert!(
            runtime
                .apply_host(
                    handle,
                    patch(HostLock::Caps, HostValue::Known(false)),
                    4,
                    clock.time(1),
                    clock.time(10),
                    clock.at(1)
                )
                .is_err()
        );
        let (replacement, _) = runtime.renew_host(handle, clock.at(2)).unwrap();
        assert_eq!(replacement.id(), handle.id());
        assert!(
            runtime
                .apply_host(
                    handle,
                    HostPatch::default(),
                    5,
                    clock.time(2),
                    clock.time(10),
                    clock.at(2)
                )
                .is_err()
        );
        runtime
            .apply_host(
                replacement,
                patch(HostLock::Caps, HostValue::Known(false)),
                1,
                clock.time(2),
                clock.time(10),
                clock.at(2),
            )
            .unwrap();
        assert_eq!(
            host(&runtime, replacement).fields[&HostLock::Caps].quality(),
            FieldQuality::Fresh
        );
    }
}

#[test]
fn per_lock_unavailability_is_ordered_and_preserves_last_known_without_poisoning_newer_values() {
    let (mut runtime, clock) = setup();
    let (handle, _) = runtime.register_host(scope(), clock.at(0)).unwrap();
    runtime
        .apply_host(
            handle,
            patch(HostLock::Caps, HostValue::Known(true)),
            1,
            clock.time(0),
            clock.time(10),
            clock.at(0),
        )
        .unwrap();
    let known = host(&runtime, handle).fields[&HostLock::Caps]
        .last_known
        .clone();
    runtime
        .apply_host(
            handle,
            patch(
                HostLock::Caps,
                HostValue::Unavailable(HostUnavailable::Unsupported),
            ),
            2,
            clock.time(1),
            clock.time(10),
            clock.at(1),
        )
        .unwrap();
    let unavailable = host(&runtime, handle);
    assert_eq!(unavailable.availability, SourceAvailability::Active);
    assert_eq!(unavailable.fields[&HostLock::Caps].last_known, known);
    assert_eq!(
        unavailable.fields[&HostLock::Caps].quality(),
        FieldQuality::Failed
    );
    assert_eq!(
        unavailable.fields[&HostLock::Caps].failure.unwrap().cause,
        SourceFailure::HostUnsupported
    );
    runtime
        .apply_host(
            handle,
            patch(HostLock::Caps, HostValue::Known(false)),
            3,
            clock.time(2),
            clock.time(10),
            clock.at(2),
        )
        .unwrap();
    let outcome = runtime
        .apply_host(
            handle,
            patch(
                HostLock::Caps,
                HostValue::Unavailable(HostUnavailable::PermissionDenied),
            ),
            2,
            clock.time(1),
            clock.time(10),
            clock.at(3),
        )
        .unwrap();
    assert_eq!(outcome.ignored_stale_fields, 1);
    let current = host(&runtime, handle);
    assert!(current.fields[&HostLock::Caps].failure.is_none());
    assert!(
        !current.fields[&HostLock::Caps]
            .last_known
            .as_ref()
            .unwrap()
            .value
    );
    runtime
        .apply_host(
            handle,
            patch(
                HostLock::Num,
                HostValue::Unavailable(HostUnavailable::Unsupported),
            ),
            4,
            clock.time(3),
            clock.time(10),
            clock.at(3),
        )
        .unwrap();
    let (new, _) = runtime.renew_host(handle, clock.at(3)).unwrap();
    assert!(host(&runtime, new).fields[&HostLock::Num].failure.is_none());
    assert!(
        runtime
            .apply_host(
                handle,
                patch(HostLock::Caps, HostValue::Known(true)),
                5,
                clock.time(3),
                clock.time(10),
                clock.at(3)
            )
            .is_err()
    );
}

#[test]
fn retiring_an_input_binding_atomically_stales_its_host_and_permanently_rejects_the_old_handle() {
    let (mut runtime, clock) = setup();
    let scan = crate::test_support::input::scan();
    runtime
        .apply_input_scan(
            scan.clone(),
            crate::model::input::MonitorStatus::Watching,
            clock.at(0),
        )
        .unwrap();
    let id = runtime.input_endpoints().next().unwrap().id;
    let (handle, _) = runtime
        .register_host_for_input(scope(), id, clock.at(0))
        .unwrap();
    runtime
        .apply_host(
            handle,
            patch(HostLock::Caps, HostValue::Known(true)),
            1,
            clock.time(0),
            clock.time(10),
            clock.at(0),
        )
        .unwrap();
    let mut empty = scan.clone();
    empty.groups.clear();
    empty.endpoints.clear();
    let outcome = runtime
        .apply_input_scan(
            empty,
            crate::model::input::MonitorStatus::Watching,
            clock.at(1),
        )
        .unwrap();
    let record = outcome.commit.unwrap().upserts[&RecordKey::Host(handle.id())].clone();
    let StateRecord::Host(record) = record.as_ref() else {
        panic!("host record");
    };
    assert_eq!(
        record.availability,
        SourceAvailability::Unavailable(SourceFailure::InvalidBinding)
    );
    assert_eq!(
        record.fields[&HostLock::Caps].quality(),
        FieldQuality::Stale
    );
    assert!(runtime.renew_host(handle, clock.at(1)).is_err());
    runtime
        .apply_input_scan(
            scan,
            crate::model::input::MonitorStatus::Watching,
            clock.at(2),
        )
        .unwrap();
    assert_ne!(runtime.input_endpoints().next().unwrap().id, id);
    assert!(runtime.renew_host(handle, clock.at(2)).is_err());
    assert!(
        runtime
            .apply_host(
                handle,
                patch(HostLock::Caps, HostValue::Known(false)),
                2,
                clock.time(2),
                clock.time(10),
                clock.at(2)
            )
            .is_err()
    );
}

#[test]
fn expiry_is_an_explicit_single_commit_at_the_deadline_and_snapshot_reads_are_pure() {
    let (mut runtime, clock) = setup();
    let (handle, _) = runtime.register_host(scope(), clock.at(0)).unwrap();
    runtime
        .apply_host(
            handle,
            patch(HostLock::Caps, HostValue::Known(true)),
            1,
            clock.time(0),
            clock.time(2),
            clock.at(0),
        )
        .unwrap();
    let before = runtime.reader().snapshot().unwrap();
    clock.at(2);
    assert_eq!(runtime.reader().snapshot().unwrap(), before);
    assert_eq!(runtime.next_deadline(), Some(clock.time(2)));
    let expired = runtime.advance_time(clock.at(2)).unwrap().commit.unwrap();
    assert_eq!(expired.cursor.revision, before.cursor.revision + 1);
    assert_eq!(
        host(&runtime, handle).fields[&HostLock::Caps].quality(),
        FieldQuality::Stale
    );
    assert!(runtime.next_deadline().is_none());
    assert!(runtime.advance_time(clock.at(2)).unwrap().commit.is_none());
}

#[test]
fn delayed_acceptance_does_not_renew_an_already_expired_report() {
    let (mut runtime, clock) = setup();
    let (handle, _) = runtime.register_host(scope(), clock.at(0)).unwrap();
    runtime
        .apply_host(
            handle,
            patch(HostLock::Caps, HostValue::Known(true)),
            1,
            clock.time(0),
            clock.time(2),
            clock.at(5),
        )
        .unwrap();
    let state = host(&runtime, handle);
    let field = &state.fields[&HostLock::Caps];
    assert_eq!(field.quality(), FieldQuality::Stale);
    assert_eq!(
        field.report.as_ref().unwrap().stamp.received_at,
        Timestamp::ZERO
    );
    assert_eq!(
        field.report.as_ref().unwrap().stamp.fresh_until,
        Timestamp(Duration::from_secs(2))
    );
}

#[test]
fn per_leaf_ordering_accepts_unrelated_older_fields_but_ignores_duplicates() {
    let (mut runtime, clock) = setup();
    let (handle, _) = runtime.register_host(scope(), clock.at(0)).unwrap();
    runtime
        .apply_host(
            handle,
            patch(HostLock::Caps, HostValue::Known(true)),
            2,
            clock.time(1),
            clock.time(10),
            clock.at(2),
        )
        .unwrap();
    runtime
        .apply_host(
            handle,
            patch(HostLock::Num, HostValue::Known(false)),
            1,
            clock.time(0),
            clock.time(10),
            clock.at(2),
        )
        .unwrap();
    let before = runtime.reader().snapshot().unwrap();
    let duplicate = runtime
        .apply_host(
            handle,
            patch(HostLock::Caps, HostValue::Known(true)),
            2,
            clock.time(1),
            clock.time(10),
            clock.at(2),
        )
        .unwrap();
    assert_eq!(duplicate.ignored_stale_fields, 1);
    assert!(duplicate.commit.is_none());
    assert_eq!(runtime.reader().snapshot().unwrap(), before);
    let refresh = runtime
        .apply_host(
            handle,
            patch(HostLock::Caps, HostValue::Known(true)),
            3,
            clock.time(3),
            clock.time(11),
            clock.at(3),
        )
        .unwrap();
    assert!(refresh.commit.is_some());
}

#[test]
fn regressing_action_or_source_clocks_and_invalid_deadlines_leave_state_unchanged() {
    let (mut runtime, clock) = setup();
    let (handle, _) = runtime.register_host(scope(), clock.at(0)).unwrap();
    runtime
        .apply_host(
            handle,
            patch(HostLock::Caps, HostValue::Known(true)),
            16,
            clock.time(19),
            clock.time(30),
            clock.at(20),
        )
        .unwrap();
    runtime
        .apply_host(
            handle,
            patch(HostLock::Num, HostValue::Known(true)),
            20,
            clock.time(20),
            clock.time(30),
            clock.at(20),
        )
        .unwrap();
    let before = runtime.reader().snapshot().unwrap();
    assert!(
        runtime
            .apply_host(
                handle,
                patch(HostLock::Caps, HostValue::Known(false)),
                19,
                clock.time(18),
                clock.time(30),
                clock.at(20)
            )
            .is_err()
    );
    assert!(runtime.advance_time(clock.at(19)).is_err());
    clock.at(20);
    assert!(
        runtime
            .apply_host(
                handle,
                HostPatch::default(),
                21,
                clock.time(21),
                clock.time(30),
                clock.time(20)
            )
            .is_err()
    );
    assert!(
        runtime
            .apply_host(
                handle,
                HostPatch::default(),
                21,
                clock.time(20),
                clock.time(19),
                clock.time(20)
            )
            .is_err()
    );
    assert!(runtime.advance_time(clock.time(21)).is_err());
    assert_eq!(runtime.reader().snapshot().unwrap(), before);
}

#[test]
fn retirement_removes_only_its_owned_projection_and_preserves_the_other_half() {
    let (mut runtime, clock) = setup();
    let mut left_spec = unit();
    left_spec.components = vec![ComponentSpec {
        name: None,
        kind: ComponentKind::Half(Some(model::topology::Side::Left)),
        parent: None,
    }];
    let (left, _) = runtime
        .add_physical_device(None, left_spec, clock.at(0))
        .unwrap();
    let keyboard = runtime.topology().physical_device(left).unwrap().keyboard();
    let mut right_spec = unit();
    right_spec.components = vec![ComponentSpec {
        name: None,
        kind: ComponentKind::Half(Some(model::topology::Side::Right)),
        parent: None,
    }];
    let (right, _) = runtime
        .add_physical_device(Some(keyboard), right_spec, clock.at(0))
        .unwrap();
    let left_component = runtime.topology().components(left).next().unwrap().id();
    let (module, _) = runtime
        .add_component(
            left,
            Some(left_component),
            ComponentKind::Module,
            None,
            clock.at(0),
        )
        .unwrap();
    let (_, mut sub) = runtime.reader().subscribe_and_snapshot(4).unwrap();
    let removed = runtime
        .remove_component(module, clock.at(0))
        .unwrap()
        .commit
        .unwrap();
    assert_eq!(
        removed.removals,
        std::collections::BTreeSet::from([RecordKey::Component(module)])
    );
    runtime.retire_physical_device(left, clock.at(0)).unwrap();
    assert_eq!(runtime.topology().members(keyboard).count(), 1);
    assert!(runtime.topology().physical_device(right).is_ok());
    assert!(
        runtime
            .reader()
            .snapshot()
            .unwrap()
            .records
            .contains_key(&RecordKey::Keyboard(keyboard))
    );
    runtime.retire_physical_device(right, clock.at(0)).unwrap();
    assert!(runtime.reader().snapshot().unwrap().records.is_empty());
    assert!(
        sub.poll_next(&mut std::task::Context::from_waker(std::task::Waker::noop()))
            .is_ready()
    );
}

#[test]
fn failed_topology_mutations_and_failed_epoch_allocation_are_atomic() {
    let (mut runtime, clock) = setup();
    let before = runtime.reader().snapshot().unwrap();
    let mut invalid = unit();
    invalid.components = vec![ComponentSpec {
        name: None,
        kind: ComponentKind::Module,
        parent: Some(0),
    }];
    assert!(
        runtime
            .add_physical_device(None, invalid, clock.at(0))
            .is_err()
    );
    assert_eq!(runtime.reader().snapshot().unwrap(), before);
    runtime.store.force_revision(u64::MAX);
    runtime.store = {
        let mut store = StateStore::new(
            before.records,
            Box::new(Epochs {
                next: 1,
                fail_at: Some(2),
            }),
        )
        .unwrap();
        store.force_revision(u64::MAX);
        store
    };
    let before = runtime.reader().snapshot().unwrap();
    assert!(
        runtime
            .add_physical_device(None, unit(), clock.at(0))
            .is_err()
    );
    assert_eq!(runtime.reader().snapshot().unwrap(), before);
    assert_eq!(runtime.topology().physical_devices().count(), 0);
}

#[test]
fn interface_projection_is_self_contained_and_identical_renames_do_not_commit() {
    let (mut runtime, clock) = setup();
    let mut spec = unit();
    spec.interfaces = vec![InterfaceSpec {
        name: None,
        usb_number: Some(3),
    }];
    spec.endpoints = vec![EndpointSpec {
        interface: Some(0),
        transport: model::connection::TransportKind::Hid,
        locator: model::connection::EndpointLocator::new("fake").unwrap(),
        hid: Default::default(),
    }];
    let (physical, _) = runtime
        .add_physical_device(None, spec, clock.at(0))
        .unwrap();
    let keyboard = runtime
        .topology()
        .physical_device(physical)
        .unwrap()
        .keyboard();
    let name = runtime
        .topology()
        .keyboard(keyboard)
        .unwrap()
        .name()
        .clone();
    assert!(
        runtime
            .rename_keyboard(keyboard, name, clock.at(0))
            .unwrap()
            .commit
            .is_none()
    );
    let snapshot = runtime.reader().snapshot().unwrap();
    for record in snapshot.records.values() {
        if let StateRecord::Endpoint {
            interface: Some(id),
            ..
        } = record.as_ref()
        {
            assert!(snapshot.records.contains_key(&RecordKey::Interface(*id)));
        }
    }
}

#[test]
fn retiring_a_host_source_removes_its_status_and_ordering_state() {
    let (mut runtime, clock) = setup();
    let (handle, _) = runtime.register_host(scope(), clock.at(0)).unwrap();
    runtime
        .apply_host(
            handle,
            patch(HostLock::Caps, HostValue::Known(true)),
            1,
            clock.time(0),
            clock.time(10),
            clock.at(0),
        )
        .unwrap();
    let removed = runtime
        .retire_host(handle, clock.at(0))
        .unwrap()
        .commit
        .unwrap();
    assert!(removed.removals.contains(&RecordKey::Host(handle.id())));
    assert!(runtime.host_frontiers.is_empty());
    assert!(
        runtime
            .apply_host(
                handle,
                HostPatch::default(),
                2,
                clock.time(0),
                clock.time(10),
                clock.at(0)
            )
            .is_err()
    );
}

#[test]
fn normalized_status_constructors_preserve_empty_unknown_and_zero_as_different_values() {
    use crate::model::observation::{DeviceField, DevicePatch, LightingZone, StatusValue};
    let empty = DevicePatch::layers(Observation::Known(vec![])).unwrap();
    let unknown = DevicePatch::layers(Observation::Unknown).unwrap();
    assert_ne!(empty, unknown);
    assert_eq!(
        empty.fields()[&DeviceField::ActiveLayers].value,
        Observation::Known(StatusValue::Layers(Default::default()))
    );
    assert_ne!(
        DevicePatch::brightness(
            LightingZone::Component,
            Observation::Known(Percentage::new(0).unwrap())
        ),
        DevicePatch::brightness(LightingZone::Component, Observation::Unknown)
    );
    let id = NamespacedId::new("source", "layer").unwrap();
    assert!(DevicePatch::layers(Observation::Known(vec![id.clone(), id])).is_err());
    assert!(empty.clone().merge(empty).is_err());
}
