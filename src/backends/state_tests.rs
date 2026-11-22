//! Fake protocol -> ingress envelopes -> runtime -> immutable snapshots/subscriptions.
use super::{fake::*, session::SessionState, *};
use crate::{
    model::{
        capability::{CapabilityOperation as Op, Support},
        connection::{EndpointLocator, TransportKind, UnavailableReason},
        identity::{HardwareIdentity, IdentityAbsence},
        observation::{
            ChargingState, DeviceField, DeviceIndicator, HostLock, LightingZone, NamespacedId,
            Observation, Percentage, ReportedUnavailable, StatusValue,
        },
        state::*,
        time::Clock,
        topology::{ComponentKind, ComponentSpec, EndpointSpec, PhysicalDeviceSpec},
    },
    runtime::Runtime,
    transport::{AccessGrant, TransportFailure},
};
use std::task::Waker;

fn drive(session: &mut Session) -> Poll<error::Result<()>> {
    session.poll_drive(&mut Context::from_waker(Waker::noop()))
}
fn start(endpoints: usize, capacity: usize) -> (Rig, Runtime, Session, events::EventReceiver) {
    let mut rig = Rig::new(endpoints, true);
    let backend = FakeBackend::new(Profile::Both, UpdateStrategy::Push);
    let candidate = rig.candidate(&backend);
    let context = rig.context();
    let leases = rig.leases(AccessGrant::Cooperative).unwrap();
    let mut options = rig.options();
    options.event_capacity = capacity;
    let mut runtime = Runtime::new(std::mem::take(&mut rig.topology), rig.clock.clone()).unwrap();
    let mut session = ready(backend.connect(candidate, context, leases, options)).unwrap();
    runtime
        .register_session(session.observer(), rig.clock.now())
        .unwrap();
    let receiver = session.take_events().unwrap();
    (rig, runtime, session, receiver)
}
fn device(
    runtime: &Runtime,
    component: crate::model::identity::ComponentId,
    endpoint: crate::model::identity::EndpointId,
) -> DeviceState {
    let snapshot = runtime.reader().snapshot().unwrap();
    let StateRecord::Device(state) =
        snapshot.records[&RecordKey::Device(component, endpoint)].as_ref()
    else {
        panic!("device source record expected");
    };
    state.as_ref().clone()
}
fn accept(
    runtime: &mut Runtime,
    observer: &SessionObserver,
    delivery: &ObservationDelivery,
    now: Instant,
) -> CommitOutcome {
    runtime
        .accept_observation(
            observer,
            delivery,
            &FreshnessDeadlines::uniform(delivery.ingress().received_at + Duration::from_secs(10)),
            now,
        )
        .unwrap()
}
fn emit(
    rig: &Rig,
    session: &mut Session,
    receiver: &mut events::EventReceiver,
    frame: Vec<u8>,
) -> ObservationDelivery {
    rig.wires[&rig.primary].push(frame);
    assert!(drive(session).is_pending());
    ready(receiver.next()).unwrap().unwrap()
}

#[test]
fn status_families_commit_partial_observed_values_without_conflating_host_locks_or_management_health()
 {
    let (rig, mut runtime, mut session, mut events) = start(1, 4);
    let observer = session.observer();
    let (host, _) = runtime
        .register_host(NamespacedId::new("seat", "one").unwrap(), rig.clock.now())
        .unwrap();
    runtime
        .apply_host(
            host,
            HostPatch(BTreeMap::from([(HostLock::Caps, HostValue::Known(true))])),
            1,
            rig.clock.now(),
            rig.deadline(),
            rig.clock.now(),
        )
        .unwrap();
    let delivery = emit(&rig, &mut session, &mut events, vec![0xaf]);
    accept(&mut runtime, &observer, &delivery, rig.clock.now());
    let state = device(&runtime, rig.component.unwrap(), rig.primary);
    assert_eq!(
        state.fields[&DeviceField::Indicator(DeviceIndicator::CapsLock)]
            .report
            .as_ref()
            .unwrap()
            .value,
        Observation::Known(StatusValue::Bool(false))
    );
    assert_eq!(
        state.fields[&DeviceField::ActiveLayers]
            .report
            .as_ref()
            .unwrap()
            .value,
        Observation::Known(StatusValue::Layers(Default::default()))
    );
    assert_eq!(
        state.fields[&DeviceField::Brightness(LightingZone::Component)]
            .report
            .as_ref()
            .unwrap()
            .value,
        Observation::Known(StatusValue::Percent(Percentage::new(0).unwrap()))
    );
    for field in state.fields.values() {
        assert_eq!(field.quality(), FieldQuality::Fresh);
        assert_eq!(
            field
                .report
                .as_ref()
                .unwrap()
                .stamp
                .measured_at
                .as_ref()
                .unwrap()
                .ticks,
            17
        );
    }
    let snapshot = runtime.reader().snapshot().unwrap();
    let StateRecord::Host(host) = snapshot.records[&RecordKey::Host(host.id())].as_ref() else {
        unreachable!();
    };
    assert_eq!(
        host.fields[&HostLock::Caps].report.as_ref().unwrap().value,
        Observation::Known(true)
    );
    assert!(matches!(
        runtime.topology().endpoint(rig.primary).unwrap().state(),
        crate::model::connection::ConnectionState::Ready(_)
    ));
}

#[test]
fn event_received_before_reply_can_apply_older_unrelated_leaves_but_cannot_overwrite_newer_values()
{
    let (rig, mut runtime, mut session, mut events) = start(1, 4);
    let observer = session.observer();
    rig.wires[&rig.primary].pause_replies();
    rig.wires[&rig.primary].push(vec![0xa7, 40]);
    rig.wires[&rig.primary].push(vec![0x94, 73]);
    let reply = ready(session.read_battery(rig.component.unwrap(), rig.deadline())).unwrap();
    let event = ready(events.next()).unwrap().unwrap();
    assert!(event.ingress().sequence < reply.ingress().sequence);
    assert_eq!(event.ingress().received_at, reply.ingress().received_at);
    accept(&mut runtime, &observer, &reply, rig.clock.now());
    let applied = accept(&mut runtime, &observer, &event, rig.clock.now());
    assert_eq!(applied.ignored_stale_fields, 1);
    assert!(applied.commit.is_some());
    let state = device(&runtime, rig.component.unwrap(), rig.primary);
    assert_eq!(
        state.fields[&DeviceField::BatteryLevel]
            .report
            .as_ref()
            .unwrap()
            .value,
        Observation::Known(StatusValue::Percent(Percentage::new(73).unwrap()))
    );
    assert_eq!(
        state.fields[&DeviceField::BatteryCharging]
            .report
            .as_ref()
            .unwrap()
            .value,
        Observation::Known(StatusValue::Charging(ChargingState::Charging))
    );
    assert_eq!(
        state.fields[&DeviceField::BatteryLevel]
            .report
            .as_ref()
            .unwrap()
            .stamp
            .sequence,
        reply.ingress().sequence
    );
    let before = runtime.reader().snapshot().unwrap();
    let duplicate = accept(&mut runtime, &observer, &event, rig.clock.now());
    assert!(duplicate.commit.is_none());
    assert_eq!(duplicate.ignored_stale_fields, 2);
    assert_eq!(runtime.reader().snapshot().unwrap(), before);
}

#[test]
fn an_earlier_firmware_event_is_not_discarded_after_a_newer_battery_reply() {
    let (rig, mut runtime, mut session, mut events) = start(1, 4);
    let observer = session.observer();
    rig.wires[&rig.primary].push(vec![0xa4]);
    let reply = ready(session.read_battery(rig.component.unwrap(), rig.deadline())).unwrap();
    let event = ready(events.next()).unwrap().unwrap();
    accept(&mut runtime, &observer, &reply, rig.clock.now());
    assert_eq!(
        accept(&mut runtime, &observer, &event, rig.clock.now()).ignored_stale_fields,
        0
    );
    let state = device(&runtime, rig.component.unwrap(), rig.primary);
    assert!(matches!(
        state.fields[&DeviceField::Firmware]
            .report
            .as_ref()
            .unwrap()
            .value,
        Observation::Known(StatusValue::Firmware(_))
    ));
}

#[test]
fn partial_device_updates_preserve_omitted_charging_metadata_and_deadlines() {
    let (rig, mut runtime, mut session, mut events) = start(1, 4);
    let observer = session.observer();
    let full = ready(session.read_battery(rig.component.unwrap(), rig.deadline())).unwrap();
    accept(&mut runtime, &observer, &full, rig.clock.now());
    let old = device(&runtime, rig.component.unwrap(), rig.primary).fields
        [&DeviceField::BatteryCharging]
        .clone();
    rig.clock.advance(Duration::from_secs(1));
    let partial = emit(&rig, &mut session, &mut events, vec![0xa1, 10]);
    accept(&mut runtime, &observer, &partial, rig.clock.now());
    assert_eq!(
        device(&runtime, rig.component.unwrap(), rig.primary).fields[&DeviceField::BatteryCharging],
        old
    );
}

#[test]
fn reported_unavailability_and_failed_queries_retain_known_data_with_distinct_quality() {
    let (rig, mut runtime, mut session, mut events) = start(1, 4);
    let observer = session.observer();
    let reply = ready(session.read_battery(rig.component.unwrap(), rig.deadline())).unwrap();
    accept(&mut runtime, &observer, &reply, rig.clock.now());
    let known = device(&runtime, rig.component.unwrap(), rig.primary).fields
        [&DeviceField::BatteryLevel]
        .last_known
        .clone();
    let unavailable = emit(&rig, &mut session, &mut events, vec![0xae]);
    accept(&mut runtime, &observer, &unavailable, rig.clock.now());
    let state = device(&runtime, rig.component.unwrap(), rig.primary);
    let field = &state.fields[&DeviceField::BatteryLevel];
    assert_eq!(
        field.report.as_ref().unwrap().value,
        Observation::Unavailable(ReportedUnavailable::Sleeping)
    );
    assert!(field.failure.is_none());
    assert_eq!(field.last_known, known);
    rig.wires[&rig.primary].fault(TransportFailure::Other);
    assert!(ready(session.read_battery(rig.component.unwrap(), rig.deadline())).is_err());
    runtime.synchronize(rig.clock.now()).unwrap();
    let field =
        &device(&runtime, rig.component.unwrap(), rig.primary).fields[&DeviceField::BatteryLevel];
    assert_eq!(field.quality(), FieldQuality::Failed);
    assert_eq!(field.last_known, known);
}

#[test]
fn delayed_device_acceptance_preserves_receipt_time_and_can_be_stale_on_arrival() {
    let (rig, mut runtime, mut session, mut events) = start(1, 4);
    let observer = session.observer();
    let delivery = emit(&rig, &mut session, &mut events, vec![0xa1, 10]);
    let until = delivery.ingress().received_at + Duration::from_secs(2);
    rig.clock.advance(Duration::from_secs(5));
    runtime
        .accept_observation(
            &observer,
            &delivery,
            &FreshnessDeadlines::uniform(until),
            rig.clock.now(),
        )
        .unwrap();
    let state = device(&runtime, rig.component.unwrap(), rig.primary);
    let field = &state.fields[&DeviceField::BatteryLevel];
    assert_eq!(field.quality(), FieldQuality::Stale);
    assert_eq!(
        field.report.as_ref().unwrap().stamp.received_at,
        crate::model::time::Timestamp::ZERO
    );
    assert!(
        runtime
            .advance_time(rig.clock.now())
            .unwrap()
            .commit
            .is_none()
    );
}

#[test]
fn invalid_multi_leaf_freshness_inputs_never_partially_apply() {
    let (rig, mut runtime, mut session, _events) = start(1, 4);
    let observer = session.observer();
    let delivery = ready(session.read_battery(rig.component.unwrap(), rig.deadline())).unwrap();
    let before = runtime.reader().snapshot().unwrap();
    let incomplete = FreshnessDeadlines::per_field(BTreeMap::from([(
        DeviceField::BatteryLevel,
        rig.deadline(),
    )]));
    assert!(
        runtime
            .accept_observation(&observer, &delivery, &incomplete, rig.clock.now())
            .is_err()
    );
    assert_eq!(runtime.reader().snapshot().unwrap(), before);
}

#[test]
fn an_overflow_gap_does_not_stale_a_newer_read_received_before_runtime_reconciliation() {
    let (rig, mut runtime, mut session, mut events) = start(1, 1);
    let observer = session.observer();
    let old = ready(session.read_battery(rig.component.unwrap(), rig.deadline())).unwrap();
    accept(&mut runtime, &observer, &old, rig.clock.now());
    rig.wires[&rig.primary].push(vec![0xa0, 1]);
    rig.wires[&rig.primary].push(vec![0xa0, 2]);
    assert!(drive(&mut session).is_pending());
    let fresh = ready(session.read_battery(rig.component.unwrap(), rig.deadline())).unwrap();
    accept(&mut runtime, &observer, &fresh, rig.clock.now());
    let state = device(&runtime, rig.component.unwrap(), rig.primary);
    assert!(state.continuity_lost.is_some());
    assert!(
        state
            .fields
            .values()
            .all(|field| field.quality() == FieldQuality::Fresh)
    );
    assert!(
        runtime
            .synchronize(rig.clock.now())
            .unwrap()
            .commit
            .is_none()
    );
    assert_eq!(
        ready(events.next()).unwrap().unwrap_err().kind(),
        FailureKind::ResyncRequired
    );
}

#[test]
fn further_discarded_input_advances_the_gap_past_an_earlier_successful_recovery_read() {
    let (rig, mut runtime, mut session, _events) = start(1, 1);
    let observer = session.observer();
    rig.wires[&rig.primary].push(vec![0xa0, 1]);
    rig.wires[&rig.primary].push(vec![0xa0, 2]);
    assert!(drive(&mut session).is_pending());
    let recovered = ready(session.read_battery(rig.component.unwrap(), rig.deadline())).unwrap();
    accept(&mut runtime, &observer, &recovered, rig.clock.now());
    assert!(
        device(&runtime, rig.component.unwrap(), rig.primary)
            .fields
            .values()
            .all(|field| field.quality() == FieldQuality::Fresh)
    );
    rig.wires[&rig.primary].push(vec![0xa0, 99]);
    assert!(drive(&mut session).is_pending());
    runtime.synchronize(rig.clock.now()).unwrap();
    let state = device(&runtime, rig.component.unwrap(), rig.primary);
    assert!(state.continuity_lost.unwrap().0 > recovered.ingress().sequence);
    assert!(
        state
            .fields
            .values()
            .all(|field| field.quality() == FieldQuality::Stale)
    );
}

#[test]
fn continuity_loss_stales_old_values_and_partial_recovery_does_not_refresh_omitted_fields() {
    let (rig, mut runtime, mut session, _events) = start(1, 1);
    let observer = session.observer();
    let old = ready(session.read_battery(rig.component.unwrap(), rig.deadline())).unwrap();
    accept(&mut runtime, &observer, &old, rig.clock.now());
    rig.wires[&rig.primary].push(vec![0xa0, 1]);
    rig.wires[&rig.primary].push(vec![0xa0, 2]);
    assert!(drive(&mut session).is_pending());
    runtime.synchronize(rig.clock.now()).unwrap();
    assert!(
        device(&runtime, rig.component.unwrap(), rig.primary)
            .fields
            .values()
            .all(|field| field.quality() == FieldQuality::Stale)
    );
    rig.wires[&rig.primary].pause_replies();
    rig.wires[&rig.primary].push(vec![0x94, 80]);
    let recovered = ready(session.read_battery(rig.component.unwrap(), rig.deadline())).unwrap();
    accept(&mut runtime, &observer, &recovered, rig.clock.now());
    let state = device(&runtime, rig.component.unwrap(), rig.primary);
    assert_eq!(
        state.fields[&DeviceField::BatteryLevel].quality(),
        FieldQuality::Fresh
    );
    assert_eq!(
        state.fields[&DeviceField::BatteryCharging].quality(),
        FieldQuality::Stale
    );
}

#[test]
fn session_drop_and_admitted_request_cancellation_invalidate_values_without_input_queue_space() {
    for cancel in [false, true] {
        let (rig, mut runtime, mut session, _events) = start(1, 1);
        let observer = session.observer();
        let old = ready(session.read_battery(rig.component.unwrap(), rig.deadline())).unwrap();
        accept(&mut runtime, &observer, &old, rig.clock.now());
        rig.wires[&rig.primary].push(vec![0xa0, 1]);
        assert!(drive(&mut session).is_pending());
        if cancel {
            rig.wires[&rig.primary].pause_replies();
            let mut request = session.read_battery(rig.component.unwrap(), rig.deadline());
            assert!(poll_once(request.as_mut()).is_pending());
            drop(request);
            assert_eq!(session.state(), SessionState::Failed);
        }
        drop(session);
        runtime.synchronize(rig.clock.now()).unwrap();
        let state = device(&runtime, rig.component.unwrap(), rig.primary);
        assert_ne!(state.availability, SourceAvailability::Active);
        assert!(
            state
                .fields
                .values()
                .all(|field| field.quality() != FieldQuality::Fresh)
        );
        assert!(
            runtime
                .accept_observation(
                    &observer,
                    &old,
                    &FreshnessDeadlines::uniform(rig.deadline()),
                    rig.clock.now()
                )
                .is_err()
        );
        rig.assert_released();
    }
}

#[test]
fn access_loss_and_revalidation_change_state_without_turning_capability_support_into_false() {
    for reason in [
        UnavailableReason::Busy,
        UnavailableReason::PermissionDenied,
        UnavailableReason::Suspended,
    ] {
        let (rig, mut runtime, mut session, _events) = start(1, 4);
        let observer = session.observer();
        let scope = crate::model::capability::CapabilityScope::Keyboard(rig.keyboard);
        let prepared = runtime.prepare_capabilities(&session, scope).unwrap();
        let report = ready(session.query_capabilities(scope, rig.deadline())).unwrap();
        runtime
            .accept_capabilities(&session, prepared, report)
            .unwrap();
        let delivery = ready(session.read_battery(rig.component.unwrap(), rig.deadline())).unwrap();
        accept(&mut runtime, &observer, &delivery, rig.clock.now());
        let outcome = runtime
            .invalidate_endpoint(rig.primary, reason, rig.clock.now())
            .unwrap();
        assert!(outcome.commit.is_some());
        assert!(
            device(&runtime, rig.component.unwrap(), rig.primary)
                .fields
                .values()
                .all(|field| field.quality() == FieldQuality::Stale)
        );
        if reason != UnavailableReason::Suspended {
            assert_eq!(
                runtime
                    .topology()
                    .capabilities(scope)
                    .unwrap()
                    .support(Op::ReadBattery),
                Support::Supported
            );
        }
        runtime
            .revalidate_endpoint(rig.primary, rig.clock.now())
            .unwrap();
        assert_eq!(
            runtime
                .topology()
                .capabilities(scope)
                .unwrap()
                .support(Op::ReadBattery),
            Support::Unknown
        );
        assert!(
            device(&runtime, rig.component.unwrap(), rig.primary)
                .fields
                .values()
                .all(|field| field.quality() != FieldQuality::Fresh)
        );
        assert!(
            runtime
                .accept_observation(
                    &observer,
                    &delivery,
                    &FreshnessDeadlines::uniform(rig.deadline()),
                    rig.clock.now()
                )
                .is_err()
        );
    }
}

#[test]
fn capability_cache_updates_do_not_refresh_or_commit_observed_state() {
    let (rig, mut runtime, mut session, _events) = start(1, 4);
    let observer = session.observer();
    let delivery = ready(session.read_battery(rig.component.unwrap(), rig.deadline())).unwrap();
    accept(&mut runtime, &observer, &delivery, rig.clock.now());
    let before = runtime.reader().snapshot().unwrap();
    let scope = crate::model::capability::CapabilityScope::Keyboard(rig.keyboard);
    let prepared = runtime.prepare_capabilities(&session, scope).unwrap();
    let report = ready(session.query_capabilities(scope, rig.deadline())).unwrap();
    runtime
        .accept_capabilities(&session, prepared, report)
        .unwrap();
    runtime.invalidate_capabilities(rig.keyboard).unwrap();
    assert_eq!(runtime.reader().snapshot().unwrap(), before);
}

#[test]
fn a_read_only_observer_can_commit_an_event_while_a_request_still_borrows_the_session() {
    let (rig, mut runtime, mut session, mut events) = start(1, 4);
    let observer = session.observer();
    rig.wires[&rig.primary].pause_replies();
    let mut request = session.read_firmware(rig.component.unwrap(), rig.deadline());
    assert!(poll_once(request.as_mut()).is_pending());
    rig.wires[&rig.primary].push(vec![0xa1, 42]);
    assert!(poll_once(request.as_mut()).is_pending());
    let event = ready(events.next()).unwrap().unwrap();
    assert!(
        accept(&mut runtime, &observer, &event, rig.clock.now())
            .commit
            .is_some()
    );
    rig.wires[&rig.primary].push(vec![0x91, b'v']);
    let reply = ready(request).unwrap();
    accept(&mut runtime, &observer, &reply, rig.clock.now());
}

#[test]
fn reconnect_replaces_the_source_generation_without_accumulating_session_history() {
    let (mut rig, mut runtime, mut session, _events) = start(1, 4);
    let backend = FakeBackend::new(Profile::Both, UpdateStrategy::Push);
    let original = ready(session.read_battery(rig.component.unwrap(), rig.deadline())).unwrap();
    let old_observer = session.observer();
    accept(&mut runtime, &old_observer, &original, rig.clock.now());
    for _ in 0..8 {
        session.disconnect();
        runtime.synchronize(rig.clock.now()).unwrap();
        for endpoint in &rig.endpoints {
            runtime
                .revalidate_endpoint(*endpoint, rig.clock.now())
                .unwrap();
        }
        rig.reopen_wires();
        let context = ConnectContext::new(
            runtime.topology(),
            rig.primary,
            rig.endpoints.clone(),
            vec![(rig.component.unwrap(), rig.primary)],
        )
        .unwrap();
        let leases = rig
            .leases_from(runtime.topology(), AccessGrant::Cooperative)
            .unwrap();
        session = ready(session.reconnect(
            &backend,
            rig.candidate(&backend),
            context,
            leases,
            rig.options(),
        ))
        .unwrap();
        runtime
            .register_session(session.observer(), rig.clock.now())
            .unwrap();
        assert!(
            device(&runtime, rig.component.unwrap(), rig.primary)
                .fields
                .values()
                .all(|field| field.quality() != FieldQuality::Fresh)
        );
        let fresh = ready(session.read_battery(rig.component.unwrap(), rig.deadline())).unwrap();
        accept(&mut runtime, &session.observer(), &fresh, rig.clock.now());
        let snapshot = runtime.reader().snapshot().unwrap();
        assert_eq!(
            snapshot
                .records
                .keys()
                .filter(|key| matches!(key, RecordKey::Session(_)))
                .count(),
            1
        );
        assert_eq!(
            snapshot
                .records
                .keys()
                .filter(|key| matches!(key, RecordKey::Device(..)))
                .count(),
            1
        );
    }
    assert!(
        runtime
            .accept_observation(
                &session.observer(),
                &original,
                &FreshnessDeadlines::uniform(rig.deadline()),
                rig.clock.now()
            )
            .is_err()
    );
    assert!(
        runtime
            .accept_observation(
                &old_observer,
                &original,
                &FreshnessDeadlines::uniform(rig.deadline()),
                rig.clock.now()
            )
            .is_err()
    );
}

#[test]
fn retiring_one_physical_member_stales_surviving_routes_of_the_invalidated_whole_session() {
    let mut rig = Rig::new(1, true);
    let right = rig
        .topology
        .add_physical_device(
            Some(rig.keyboard),
            PhysicalDeviceSpec {
                name: None,
                identity: HardwareIdentity::missing(IdentityAbsence::NotReported),
                usb_ids: None,
                serial: None,
                interfaces: vec![],
                endpoints: vec![EndpointSpec {
                    interface: None,
                    transport: TransportKind::Serial,
                    locator: EndpointLocator::new("right").unwrap(),
                    hid: Default::default(),
                }],
                components: vec![ComponentSpec {
                    name: None,
                    kind: ComponentKind::Half(Some(crate::model::topology::Side::Right)),
                    parent: None,
                }],
            },
        )
        .unwrap();
    let right_endpoint = rig.topology.endpoints(right).next().unwrap().id();
    let right_component = rig.topology.components(right).next().unwrap().id();
    rig.topology.revalidate_endpoint(right_endpoint).unwrap();
    rig.endpoints.push(right_endpoint);
    rig.reopen_wires();
    let left = rig.topology.endpoint(rig.primary).unwrap().physical();
    let context = ConnectContext::new(
        &rig.topology,
        rig.primary,
        rig.endpoints.clone(),
        vec![
            (rig.component.unwrap(), rig.primary),
            (right_component, right_endpoint),
        ],
    )
    .unwrap();
    let leases = rig.leases(AccessGrant::Cooperative).unwrap();
    let backend = FakeBackend::new(Profile::Both, UpdateStrategy::Push);
    let mut runtime = Runtime::new(std::mem::take(&mut rig.topology), rig.clock.clone()).unwrap();
    let mut session =
        ready(backend.connect(rig.candidate(&backend), context, leases, rig.options())).unwrap();
    let observer = session.observer();
    runtime
        .register_session(observer.clone(), rig.clock.now())
        .unwrap();
    let right_delivery = ready(session.read_battery(right_component, rig.deadline())).unwrap();
    accept(&mut runtime, &observer, &right_delivery, rig.clock.now());
    runtime
        .retire_physical_device(left, rig.clock.now())
        .unwrap();
    assert!(runtime.topology().physical_device(right).is_ok());
    assert!(runtime.topology().keyboard(rig.keyboard).is_ok());
    assert!(
        device(&runtime, right_component, right_endpoint)
            .fields
            .values()
            .all(|field| field.quality() != FieldQuality::Fresh)
    );
    let before = runtime.reader().snapshot().unwrap();
    assert!(
        runtime
            .accept_observation(
                &observer,
                &right_delivery,
                &FreshnessDeadlines::uniform(rig.deadline()),
                rig.clock.now()
            )
            .is_err()
    );
    assert_eq!(runtime.reader().snapshot().unwrap(), before);
    runtime
        .retire_physical_device(right, rig.clock.now())
        .unwrap();
    assert!(runtime.reader().snapshot().unwrap().records.is_empty());
}

#[test]
fn two_endpoint_sources_for_one_component_remain_separate_without_averaging() {
    let mut rig = Rig::new(2, true);
    let backend = FakeBackend::new(Profile::Both, UpdateStrategy::Push);
    let mut sessions = Vec::new();
    for endpoint in &rig.endpoints {
        let context = ConnectContext::new(
            &rig.topology,
            *endpoint,
            vec![*endpoint],
            vec![(rig.component.unwrap(), *endpoint)],
        )
        .unwrap();
        let leases = rig
            .leases_for(&rig.topology, &[*endpoint], AccessGrant::Cooperative)
            .unwrap();
        let candidate = BackendCandidate::new(
            backend.descriptor().id(),
            rig.candidate(&backend).configuration(),
            vec![*endpoint],
            vec![MatchEvidence::Transport(TransportKind::Serial)],
        )
        .unwrap();
        sessions.push(ready(backend.connect(candidate, context, leases, rig.options())).unwrap());
    }
    let mut runtime = Runtime::new(std::mem::take(&mut rig.topology), rig.clock.clone()).unwrap();
    for session in &sessions {
        runtime
            .register_session(session.observer(), rig.clock.now())
            .unwrap();
    }
    let a = ready(sessions[0].read_battery(rig.component.unwrap(), rig.deadline())).unwrap();
    accept(&mut runtime, &sessions[0].observer(), &a, rig.clock.now());
    rig.wires[&rig.endpoints[1]].pause_replies();
    rig.wires[&rig.endpoints[1]].push(vec![0x94, 11]);
    let b = ready(sessions[1].read_battery(rig.component.unwrap(), rig.deadline())).unwrap();
    accept(&mut runtime, &sessions[1].observer(), &b, rig.clock.now());
    assert_ne!(
        device(&runtime, rig.component.unwrap(), rig.endpoints[0]).fields
            [&DeviceField::BatteryLevel]
            .report
            .as_ref()
            .unwrap()
            .value,
        device(&runtime, rig.component.unwrap(), rig.endpoints[1]).fields
            [&DeviceField::BatteryLevel]
            .report
            .as_ref()
            .unwrap()
            .value
    );
    assert_eq!(
        runtime
            .reader()
            .snapshot()
            .unwrap()
            .records
            .keys()
            .filter(|key| matches!(key, RecordKey::Device(..)))
            .count(),
        2
    );
}
