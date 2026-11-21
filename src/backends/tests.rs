use super::fake::*;
use super::session::SessionState;
use super::*;
use crate::{
    model::{
        capability::{CapabilityDiscovery, CapabilityOperation as Op, CapabilityScope, Support},
        connection::UnavailableReason,
        observation::{DeviceObservation, Observation},
        topology::PhysicalDeviceSpec,
    },
    transport::{AccessGrant, TransportFailure},
};
use std::{error::Error as _, sync::atomic::Ordering};

fn backend(profile: Profile) -> FakeBackend {
    FakeBackend::new(profile, UpdateStrategy::Push)
}
fn drive(session: &mut Session) -> Poll<error::Result<()>> {
    let waker = std::task::Waker::from(Arc::new(WakeCounter::default()));
    session.poll_drive(&mut Context::from_waker(&waker))
}
fn empty_unit() -> PhysicalDeviceSpec {
    PhysicalDeviceSpec {
        name: None,
        identity: crate::model::identity::HardwareIdentity::missing(
            crate::model::identity::IdentityAbsence::NotReported,
        ),
        usb_ids: None,
        interfaces: vec![],
        endpoints: vec![],
        components: vec![],
    }
}

#[test]
fn independent_feature_backends_use_the_shared_session_without_unrelated_read_interfaces() {
    for profile in [Profile::Battery, Profile::Firmware] {
        let rig = Rig::new(1, true);
        let backend = FakeBackend::new(profile, UpdateStrategy::Push);
        let mut session = rig.connect(&backend, rig.candidate(&backend));
        assert_eq!(session.state(), SessionState::Active);
        match profile {
            Profile::Battery => {
                assert!(session.battery().is_some());
                assert!(session.firmware().is_none());
            }
            Profile::Firmware => {
                assert!(session.firmware().is_some());
                assert!(session.battery().is_none());
            }
            _ => unreachable!(),
        }
        session.shutdown();
        session.shutdown();
        rig.assert_released();
    }
}

#[test]
fn metadata_matching_preserves_ambiguity_without_opening_or_querying_hardware() {
    let rig = Rig::new(1, true);
    let physical = rig.topology.endpoint(rig.primary).unwrap().physical();
    let mut view = DiscoveryView::new(&rig.topology, physical).unwrap();
    let battery = backend(Profile::Battery);
    let firmware = backend(Profile::Firmware);
    let matches = MatchResolution::from_hints([
        battery.match_metadata(&view),
        firmware.match_metadata(&view),
    ]);
    let MatchResolution::Ambiguous(candidates) = matches else {
        panic!("unresolved candidates must stay ambiguous");
    };
    assert_eq!(candidates.len(), 2);
    assert!(matches!(
        MatchResolution::from_hints([battery.match_metadata(&view)]),
        MatchResolution::Candidate(_)
    ));
    view.endpoints.clear();
    assert!(matches!(battery.match_metadata(&view), MatchHint::NoMatch));
    assert!(matches!(
        MatchResolution::from_hints([MatchHint::NoMatch]),
        MatchResolution::NoMatch
    ));
    assert_eq!(rig.wires[&rig.primary].writes(), 0);
    assert!(rig.log.lock().unwrap().is_empty());
}

#[test]
fn discovery_does_not_guarantee_that_connection_will_succeed() {
    let rig = Rig::new(1, true);
    let backend = backend(Profile::Battery);
    let candidate = rig.candidate(&backend);
    rig.wires[&rig.primary].clear_frames();
    rig.wires[&rig.primary].fault(TransportFailure::Disconnected);
    let error = ready(backend.connect(
        candidate,
        rig.context(),
        rig.leases(AccessGrant::Cooperative).unwrap(),
        rig.options(),
    ))
    .unwrap_err();
    assert_eq!(error.kind(), FailureKind::Disconnected);
    assert_eq!(error.endpoint(), Some(rig.primary));
    assert!(
        error
            .source()
            .unwrap()
            .to_string()
            .contains("transport failure")
    );
    assert!(
        error
            .source()
            .unwrap()
            .source()
            .unwrap()
            .source()
            .unwrap()
            .to_string()
            .contains("synthetic device")
    );
    rig.assert_released();
}

#[test]
fn unpolled_and_pending_connect_futures_both_release_all_transferred_leases() {
    for pending in [false, true] {
        let rig = Rig::new(2, true);
        let backend = backend(Profile::Both);
        if pending {
            rig.wires[&rig.primary].clear_frames();
        }
        let mut future = backend.connect(
            rig.candidate(&backend),
            rig.context(),
            rig.leases(AccessGrant::Cooperative).unwrap(),
            rig.options(),
        );
        if pending {
            assert!(poll_once(future.as_mut()).is_pending());
        }
        drop(future);
        rig.assert_released();
    }
}

#[test]
fn a_failed_second_endpoint_handshake_closes_every_channel_before_ownership_releases() {
    let rig = Rig::new(2, true);
    let backend = backend(Profile::Both);
    let second = rig.endpoints[1];
    rig.wires[&second].clear_frames();
    rig.wires[&second].fault(TransportFailure::Disconnected);
    let error = ready(backend.connect(
        rig.candidate(&backend),
        rig.context(),
        rig.leases(AccessGrant::Cooperative).unwrap(),
        rig.options(),
    ))
    .unwrap_err();
    assert_eq!(error.endpoint(), Some(second));
    rig.assert_released();
}

#[test]
fn protocol_exclusivity_requirements_are_checked_against_the_openers_grant() {
    let rig = Rig::new(1, true);
    let mut backend = backend(Profile::Both);
    backend.exclusive = true;
    let error = ready(backend.connect(
        rig.candidate(&backend),
        rig.context(),
        rig.leases(AccessGrant::Cooperative).unwrap(),
        rig.options(),
    ))
    .unwrap_err();
    assert_eq!(error.kind(), FailureKind::ExclusiveAccess);
    assert_eq!(rig.wires[&rig.primary].writes(), 0);
    rig.assert_released();
}

#[test]
fn a_live_session_retains_its_cooperative_reservation_until_shutdown() {
    let rig = Rig::new(1, true);
    let backend = backend(Profile::Battery);
    let mut session = rig.connect(&backend, rig.candidate(&backend));
    assert!(rig.leases(AccessGrant::Cooperative).is_err());
    session.shutdown();
    assert_eq!(session.state(), SessionState::Shutdown);
    let leases = rig.leases(AccessGrant::Cooperative).unwrap();
    drop(leases);
    assert!(ready(session.read_battery(rig.component.unwrap(), rig.deadline())).is_err());
    assert_eq!(rig.wires[&rig.primary].writes(), 0);
    rig.assert_released();
}

#[test]
fn retaining_an_event_receiver_does_not_prolong_transport_ownership_after_session_drop() {
    let rig = Rig::new(2, true);
    let backend = backend(Profile::Both);
    let mut session = rig.connect(&backend, rig.candidate(&backend));
    let mut receiver = session.take_events().unwrap();
    drop(session);
    rig.assert_released();
    assert!(ready(receiver.next()).is_none());
}

#[test]
fn dropping_an_unpolled_read_preserves_the_session_but_cancelling_admitted_work_closes_it() {
    let rig = Rig::new(1, true);
    let backend = backend(Profile::Battery);
    let mut session = rig.connect(&backend, rig.candidate(&backend));
    drop(session.read_battery(rig.component.unwrap(), rig.deadline()));
    assert_eq!(session.state(), SessionState::Active);
    assert_eq!(rig.wires[&rig.primary].writes(), 0);
    let mut events = session.take_events().unwrap();
    rig.wires[&rig.primary].pause_replies();
    let mut request = session.read_battery(rig.component.unwrap(), rig.deadline());
    assert!(poll_once(request.as_mut()).is_pending());
    assert_eq!(rig.wires[&rig.primary].writes(), 1);
    drop(request);
    assert_eq!(session.state(), SessionState::Failed);
    assert_eq!(
        ready(events.next()).unwrap().unwrap_err().kind(),
        FailureKind::Cancelled
    );
    assert!(ready(events.next()).is_none());
    rig.assert_released();
}

#[test]
fn deadlines_require_host_wakeups_and_expired_unadmitted_requests_send_nothing() {
    let rig = Rig::new(1, true);
    let backend = backend(Profile::Battery);
    let mut session = rig.connect(&backend, rig.candidate(&backend));
    let error = ready(session.read_battery(rig.component.unwrap(), rig.clock.now())).unwrap_err();
    assert_eq!(error.kind(), FailureKind::DeadlineExceeded);
    assert_eq!(session.state(), SessionState::Active);
    assert_eq!(rig.wires[&rig.primary].writes(), 0);
    rig.wires[&rig.primary].pause_replies();
    let wake = Arc::new(WakeCounter::default());
    let waker = std::task::Waker::from(wake.clone());
    let mut cx = Context::from_waker(&waker);
    let mut request = session.read_battery(rig.component.unwrap(), rig.deadline());
    assert!(request.as_mut().poll(&mut cx).is_pending());
    let before = wake.0.load(Ordering::Relaxed);
    rig.clock.advance(Duration::from_secs(11));
    assert_eq!(wake.0.load(Ordering::Relaxed), before);
    let Poll::Ready(Err(error)) = request.as_mut().poll(&mut cx) else {
        panic!("repoll must observe expiration");
    };
    assert_eq!(error.kind(), FailureKind::DeadlineExceeded);
    drop(request);
    assert_eq!(session.state(), SessionState::Failed);
    rig.assert_released();
}

#[test]
fn a_connection_deadline_can_expire_before_any_negotiation() {
    let rig = Rig::new(1, true);
    let backend = backend(Profile::Battery);
    let mut options = rig.options();
    options.connect_deadline = rig.clock.now();
    let error = ready(backend.connect(
        rig.candidate(&backend),
        rig.context(),
        rig.leases(AccessGrant::Cooperative).unwrap(),
        options,
    ))
    .unwrap_err();
    assert_eq!(error.kind(), FailureKind::DeadlineExceeded);
    assert_eq!(rig.wires[&rig.primary].writes(), 0);
    rig.assert_released();
}

#[test]
fn unknown_unsupported_and_invalid_targets_are_distinct_nonmutating_dispatch_failures() {
    for (profile, expected) in [
        (Profile::Unknown, FailureKind::SupportUnknown),
        (Profile::Unsupported, FailureKind::Unsupported),
    ] {
        let rig = Rig::new(1, true);
        let backend = backend(profile);
        let mut session = rig.connect(&backend, rig.candidate(&backend));
        let error =
            ready(session.read_battery(rig.component.unwrap(), rig.deadline())).unwrap_err();
        assert_eq!(error.kind(), expected);
        assert_eq!(error.operation(), Some(Op::ReadBattery));
        assert_eq!(error.backend(), Some("synthetic.protocol"));
        assert!(error.to_string().contains("ReadBattery"));
        assert!(
            miette::Diagnostic::code(&error)
                .unwrap()
                .to_string()
                .starts_with("kbctl::backend::")
        );
        assert_eq!(session.state(), SessionState::Active);
        assert_eq!(rig.wires[&rig.primary].writes(), 0);
    }
    let rig = Rig::new(1, true);
    let foreign = Rig::new(1, true);
    let backend = backend(Profile::Battery);
    let mut session = rig.connect(&backend, rig.candidate(&backend));
    let error =
        ready(session.read_battery(foreign.component.unwrap(), rig.deadline())).unwrap_err();
    assert_eq!(error.kind(), FailureKind::InvalidTarget);
    assert_eq!(session.state(), SessionState::Active);
}

#[test]
fn pending_event_waits_do_not_block_commands_and_unsolicited_input_is_not_a_reply() {
    let rig = Rig::new(1, true);
    let backend = backend(Profile::Both);
    let mut session = rig.connect(&backend, rig.candidate(&backend));
    let mut receiver = session.take_events().unwrap();
    let mut waiting = Box::pin(receiver.next());
    assert!(poll_once(waiting.as_mut()).is_pending());
    rig.wires[&rig.primary].push(vec![0xa0, 42]);
    let firmware = ready(
        session
            .firmware()
            .unwrap()
            .read_firmware(rig.component.unwrap(), rig.deadline()),
    )
    .unwrap();
    assert!(matches!(
        session
            .accept_observation(&rig.topology, &firmware)
            .unwrap(),
        DeviceObservation::Firmware(Observation::Known(_))
    ));
    let event = ready(waiting).unwrap().unwrap();
    let DeviceObservation::Battery(state) =
        session.accept_observation(&rig.topology, &event).unwrap()
    else {
        panic!("unsolicited data must be delivered separately");
    };
    assert_eq!(
        state.level,
        Observation::Known(crate::model::observation::Percentage::new(42).unwrap())
    );
}

#[test]
fn an_unsolicited_event_can_be_received_before_a_pending_request_completes() {
    let rig = Rig::new(1, true);
    let backend = backend(Profile::Both);
    let mut session = rig.connect(&backend, rig.candidate(&backend));
    let mut receiver = session.take_events().unwrap();
    rig.wires[&rig.primary].pause_replies();
    let mut request = session.read_firmware(rig.component.unwrap(), rig.deadline());
    assert!(poll_once(request.as_mut()).is_pending());
    rig.wires[&rig.primary].push(vec![0xa0, 42]);
    assert!(poll_once(request.as_mut()).is_pending());
    let event = ready(receiver.next()).unwrap().unwrap();
    assert!(matches!(event.value(), DeviceObservation::Battery(_)));
    rig.wires[&rig.primary].push(vec![0x91, b'v']);
    let Poll::Ready(Ok(firmware)) = poll_once(request.as_mut()) else {
        panic!("the response must still complete its own request");
    };
    drop(request);
    assert!(session.accept_observation(&rig.topology, &event).is_ok());
    assert!(session.accept_observation(&rig.topology, &firmware).is_ok());
}

#[test]
fn idle_push_wakes_the_host_and_receiver_and_event_wait_cancellation_loses_nothing() {
    let rig = Rig::new(1, true);
    let backend = backend(Profile::Battery);
    let mut session = rig.connect(&backend, rig.candidate(&backend));
    let mut receiver = session.take_events().unwrap();
    let wake = Arc::new(WakeCounter::default());
    let waker = std::task::Waker::from(wake.clone());
    let mut cx = Context::from_waker(&waker);
    {
        let mut waiting = Box::pin(receiver.next());
        assert!(waiting.as_mut().poll(&mut cx).is_pending());
    }
    assert!(session.poll_drive(&mut cx).is_pending());
    rig.wires[&rig.primary].push(vec![0xa0, 55]);
    assert!(wake.0.load(Ordering::Relaxed) >= 1);
    assert!(session.poll_drive(&mut cx).is_pending());
    assert!(wake.0.load(Ordering::Relaxed) >= 2);
    let event = ready(receiver.next()).unwrap().unwrap();
    assert_eq!(event.component(), rig.component.unwrap());
    assert!(session.accept_observation(&rig.topology, &event).is_ok());
    assert_eq!(rig.wires[&rig.primary].writes(), 0);
}

#[test]
fn polling_and_push_protocols_share_typed_observations_without_a_hidden_scheduler() {
    let rig = Rig::new(1, true);
    let backend = FakeBackend::new(
        Profile::Battery,
        UpdateStrategy::Polling {
            interval: Duration::from_secs(5),
        },
    );
    let mut session = rig.connect(&backend, rig.candidate(&backend));
    assert_eq!(session.updates(), backend.descriptor().updates());
    let result = ready(
        session
            .battery()
            .unwrap()
            .read_battery(rig.component.unwrap(), rig.deadline()),
    )
    .unwrap();
    assert!(matches!(
        session.accept_observation(&rig.topology, &result).unwrap(),
        DeviceObservation::Battery(_)
    ));
    assert_eq!(rig.wires[&rig.primary].writes(), 1);
    rig.clock.advance(Duration::from_secs(5));
    assert_eq!(rig.wires[&rig.primary].writes(), 1);
}

#[test]
fn overflowing_events_reports_continuity_loss_even_when_the_queue_is_full() {
    let rig = Rig::new(1, true);
    let backend = backend(Profile::Battery);
    let mut options = rig.options();
    options.event_capacity = 1;
    let mut session = ready(backend.connect(
        rig.candidate(&backend),
        rig.context(),
        rig.leases(AccessGrant::Cooperative).unwrap(),
        options,
    ))
    .unwrap();
    let mut receiver = session.take_events().unwrap();
    rig.wires[&rig.primary].push(vec![0xa0, 1]);
    rig.wires[&rig.primary].push(vec![0xa0, 2]);
    assert!(drive(&mut session).is_pending());
    assert_eq!(
        ready(receiver.next()).unwrap().unwrap_err().kind(),
        FailureKind::ResyncRequired
    );
    assert!(ready(receiver.next()).is_none());
    assert!(session.take_events().is_err());
    assert_eq!(session.state(), SessionState::Active);
    assert!(ready(session.read_battery(rig.component.unwrap(), rig.deadline())).is_ok());
}

#[test]
fn a_required_endpoint_failure_is_visible_despite_a_full_event_queue() {
    let rig = Rig::new(2, true);
    let backend = backend(Profile::Both);
    let mut options = rig.options();
    options.event_capacity = 1;
    let mut session = ready(backend.connect(
        rig.candidate(&backend),
        rig.context(),
        rig.leases(AccessGrant::Cooperative).unwrap(),
        options,
    ))
    .unwrap();
    let mut receiver = session.take_events().unwrap();
    rig.wires[&rig.primary].push(vec![0xa0, 1]);
    assert!(drive(&mut session).is_pending());
    rig.wires[&rig.endpoints[1]].fault(TransportFailure::Disconnected);
    let Poll::Ready(Err(error)) = drive(&mut session) else {
        panic!("required endpoint failure must end the session");
    };
    assert_eq!(error.kind(), FailureKind::Disconnected);
    assert_eq!(session.state(), SessionState::Failed);
    assert_eq!(
        ready(receiver.next()).unwrap().unwrap_err().kind(),
        FailureKind::Disconnected
    );
    assert!(ready(receiver.next()).is_none());
    rig.assert_released();
}

#[test]
fn per_poll_work_limits_yield_instead_of_draining_an_unbounded_event_burst() {
    let rig = Rig::new(1, true);
    let backend = backend(Profile::Battery);
    let mut options = rig.options();
    options.work_per_poll = 1;
    let mut session = ready(backend.connect(
        rig.candidate(&backend),
        rig.context(),
        rig.leases(AccessGrant::Cooperative).unwrap(),
        options,
    ))
    .unwrap();
    let mut receiver = session.take_events().unwrap();
    rig.wires[&rig.primary].push(vec![0xa0, 1]);
    rig.wires[&rig.primary].push(vec![0xa0, 2]);
    let wake = Arc::new(WakeCounter::default());
    let waker = std::task::Waker::from(wake.clone());
    assert!(
        session
            .poll_drive(&mut Context::from_waker(&waker))
            .is_pending()
    );
    assert!(wake.0.load(Ordering::Relaxed) >= 1);
    assert!(ready(receiver.next()).unwrap().is_ok());
    let mut second = Box::pin(receiver.next());
    assert!(poll_once(second.as_mut()).is_pending());
    assert!(drive(&mut session).is_pending());
    assert!(ready(second).unwrap().is_ok());
}

#[test]
fn mismatched_replies_and_oversized_decoded_data_fail_the_session() {
    for frame in [vec![0x93], vec![0x90, 8], vec![0x91, b'v']] {
        let rig = Rig::new(1, true);
        let backend = backend(Profile::Both);
        let mut options = rig.options();
        let oversized = frame == [0x91, b'v'];
        if oversized {
            options.max_message_bytes = 2;
        }
        let mut session = ready(backend.connect(
            rig.candidate(&backend),
            rig.context(),
            rig.leases(AccessGrant::Cooperative).unwrap(),
            options,
        ))
        .unwrap();
        rig.wires[&rig.primary].pause_replies();
        rig.wires[&rig.primary].push(frame);
        let error =
            ready(session.read_firmware(rig.component.unwrap(), rig.deadline())).unwrap_err();
        assert_eq!(
            error.kind(),
            if oversized {
                FailureKind::ResourceLimit
            } else {
                FailureKind::Protocol
            }
        );
        assert_eq!(session.state(), SessionState::Failed);
        rig.assert_released();
    }
}

#[test]
fn invalid_normalized_battery_values_are_protocol_errors_instead_of_panics_or_fabricated_values() {
    let rig = Rig::new(1, true);
    let backend = backend(Profile::Battery);
    let mut session = rig.connect(&backend, rig.candidate(&backend));
    rig.wires[&rig.primary].pause_replies();
    rig.wires[&rig.primary].push(vec![0x90, 255]);
    let error = ready(session.read_battery(rig.component.unwrap(), rig.deadline())).unwrap_err();
    assert_eq!(error.kind(), FailureKind::Protocol);
    assert!(error.source().unwrap().to_string().contains("255"));
    assert_eq!(session.state(), SessionState::Failed);
    rig.assert_released();
}

#[test]
fn capability_reports_publish_through_real_refresh_tokens_without_representative_components() {
    for (profile, operation) in [
        (Profile::Battery, Op::ReadBattery),
        (Profile::Firmware, Op::ReadFirmware),
    ] {
        let mut rig = Rig::new(1, false);
        let backend = backend(profile);
        let mut session = rig.connect(&backend, rig.candidate(&backend));
        let scope = CapabilityScope::Keyboard(rig.keyboard);
        let prepared = session.prepare_refresh(&mut rig.topology, scope).unwrap();
        let report = ready(session.query_capabilities(scope, rig.deadline())).unwrap();
        session
            .accept_capabilities(&mut rig.topology, prepared, report)
            .unwrap();
        let view = rig.topology.capabilities(scope).unwrap();
        assert_eq!(view.support(operation), Support::Supported);
        assert_eq!(view.support(Op::WriteMacros), Support::Unknown);
    }
}

#[test]
fn superseded_capability_results_cannot_replace_newer_pending_work() {
    let mut rig = Rig::new(1, true);
    let backend = backend(Profile::Battery);
    let mut session = rig.connect(&backend, rig.candidate(&backend));
    let scope = CapabilityScope::Component(rig.component.unwrap());
    let old = session.prepare_refresh(&mut rig.topology, scope).unwrap();
    let new = session.prepare_refresh(&mut rig.topology, scope).unwrap();
    let old_report = ready(session.query_capabilities(scope, rig.deadline())).unwrap();
    assert_eq!(
        session
            .accept_capabilities(&mut rig.topology, old, old_report)
            .unwrap_err()
            .kind(),
        FailureKind::StaleResult
    );
    assert_eq!(
        rig.topology.capabilities(scope).unwrap().discovery(),
        CapabilityDiscovery::Refreshing
    );
    let new_report = ready(session.query_capabilities(scope, rig.deadline())).unwrap();
    session
        .accept_capabilities(&mut rig.topology, new, new_report)
        .unwrap();
    assert_eq!(
        rig.topology
            .capabilities(scope)
            .unwrap()
            .support(Op::ReadBattery),
        Support::Supported
    );
}

#[test]
fn failed_or_cancelled_capability_queries_can_abandon_their_model_refresh_without_holding_topology_across_io()
 {
    let mut rig = Rig::new(1, true);
    let backend = backend(Profile::Battery);
    let mut session = rig.connect(&backend, rig.candidate(&backend));
    let scope = CapabilityScope::Component(rig.component.unwrap());
    let prepared = session.prepare_refresh(&mut rig.topology, scope).unwrap();
    rig.wires[&rig.primary].pause_replies();
    let mut query = session.query_capabilities(scope, rig.deadline());
    assert!(poll_once(query.as_mut()).is_pending());
    assert_eq!(
        rig.topology.capabilities(scope).unwrap().discovery(),
        CapabilityDiscovery::Refreshing
    );
    drop(query);
    prepared.abandon(&mut rig.topology).unwrap();
    assert_eq!(
        rig.topology.capabilities(scope).unwrap().discovery(),
        CapabilityDiscovery::NotDiscovered
    );
    session
        .close_with_topology(&mut rig.topology, UnavailableReason::Disconnected)
        .unwrap();
    assert!(rig.topology.physical_devices().next().is_some());
}

#[test]
fn captured_delivery_bindings_reject_reattachment_removal_and_move_away_then_back() {
    for change in 0..3 {
        let mut rig = Rig::new(1, true);
        // Keep the original keyboard present during a round-trip reassociation.
        rig.topology
            .add_physical_device(Some(rig.keyboard), empty_unit())
            .unwrap();
        let destination = rig
            .topology
            .add_physical_device(None, empty_unit())
            .unwrap();
        let destination_keyboard = rig
            .topology
            .physical_device(destination)
            .unwrap()
            .keyboard();
        let backend = backend(Profile::Battery);
        let mut session = rig.connect(&backend, rig.candidate(&backend));
        let delivery = ready(session.read_battery(rig.component.unwrap(), rig.deadline())).unwrap();
        assert!(session.accept_observation(&rig.topology, &delivery).is_ok());
        match change {
            0 => {
                rig.topology.revalidate_endpoint(rig.primary).unwrap();
            }
            1 => {
                rig.topology
                    .remove_component(rig.component.unwrap())
                    .unwrap();
            }
            _ => {
                let physical = rig.topology.endpoint(rig.primary).unwrap().physical();
                rig.topology
                    .associate_physical_device(physical, destination_keyboard)
                    .unwrap();
                rig.topology
                    .associate_physical_device(physical, rig.keyboard)
                    .unwrap();
            }
        }
        assert_eq!(
            session
                .accept_observation(&rig.topology, &delivery)
                .unwrap_err()
                .kind(),
            FailureKind::StaleResult
        );
    }
}

#[test]
fn keyboard_only_bindings_reject_reassociation_without_inventing_a_component_token() {
    let mut rig = Rig::new(1, false);
    rig.topology
        .add_physical_device(Some(rig.keyboard), empty_unit())
        .unwrap();
    let destination = rig
        .topology
        .add_physical_device(None, empty_unit())
        .unwrap();
    let destination_keyboard = rig
        .topology
        .physical_device(destination)
        .unwrap()
        .keyboard();
    let backend = backend(Profile::Firmware);
    let session = rig.connect(&backend, rig.candidate(&backend));
    let physical = rig.topology.endpoint(rig.primary).unwrap().physical();
    rig.topology
        .associate_physical_device(physical, destination_keyboard)
        .unwrap();
    rig.topology
        .associate_physical_device(physical, rig.keyboard)
        .unwrap();
    let error = session
        .prepare_refresh(&mut rig.topology, CapabilityScope::Keyboard(rig.keyboard))
        .err()
        .expect("old attachment binding must not become current again");
    assert_eq!(error.kind(), FailureKind::StaleResult);
    assert_eq!(error.backend(), Some("synthetic.protocol"));
    assert_eq!(
        rig.topology
            .capabilities(CapabilityScope::Keyboard(rig.keyboard))
            .unwrap()
            .discovery(),
        CapabilityDiscovery::NotDiscovered
    );
}

#[test]
fn reconnect_uses_fresh_identity_and_attachment_context_and_rejects_old_session_events() {
    let mut rig = Rig::new(1, true);
    let backend = backend(Profile::Battery);
    let mut session = rig.connect(&backend, rig.candidate(&backend));
    let old_id = session.id();
    let mut receiver = session.take_events().unwrap();
    rig.wires[&rig.primary].push(vec![0xa0, 64]);
    assert!(drive(&mut session).is_pending());
    let old_event = ready(receiver.next()).unwrap().unwrap();
    // Opening replacements is host work and must happen only after old ownership
    // is released; the fake opener rejects a competing acquisition beforehand.
    assert!(rig.leases(AccessGrant::Cooperative).is_err());
    session.disconnect();
    let unchanged_context = rig.context();
    let error = ready(session.reconnect(
        &backend,
        rig.candidate(&backend),
        unchanged_context,
        rig.leases(AccessGrant::Cooperative).unwrap(),
        rig.options(),
    ))
    .unwrap_err();
    assert_eq!(error.kind(), FailureKind::InvalidInput);
    rig.reattach();
    let mut replacement = ready(session.reconnect(
        &backend,
        rig.candidate(&backend),
        rig.context(),
        rig.leases(AccessGrant::Cooperative).unwrap(),
        rig.options(),
    ))
    .unwrap();
    assert_ne!(replacement.id(), old_id);
    assert_eq!(
        replacement
            .accept_observation(&rig.topology, &old_event)
            .unwrap_err()
            .kind(),
        FailureKind::StaleResult
    );
    assert_eq!(session.state(), SessionState::Disconnected);
    assert!(ready(session.read_battery(rig.component.unwrap(), rig.deadline())).is_err());
    assert!(ready(replacement.read_battery(rig.component.unwrap(), rig.deadline())).is_ok());
}

#[test]
fn transport_busy_and_access_failures_preserve_the_original_cause_and_remain_distinct() {
    for (failure, expected, reason) in [
        (
            TransportFailure::Busy,
            FailureKind::Busy,
            UnavailableReason::Busy,
        ),
        (
            TransportFailure::AccessDenied,
            FailureKind::AccessDenied,
            UnavailableReason::PermissionDenied,
        ),
    ] {
        let mut rig = Rig::new(1, true);
        let backend = backend(Profile::Battery);
        let mut session = rig.connect(&backend, rig.candidate(&backend));
        let scope = CapabilityScope::Component(rig.component.unwrap());
        let prepared = session.prepare_refresh(&mut rig.topology, scope).unwrap();
        let report = ready(session.query_capabilities(scope, rig.deadline())).unwrap();
        session
            .accept_capabilities(&mut rig.topology, prepared, report)
            .unwrap();
        rig.wires[&rig.primary].pause_replies();
        rig.wires[&rig.primary].fault(failure);
        let error =
            ready(session.read_battery(rig.component.unwrap(), rig.deadline())).unwrap_err();
        assert_eq!(error.kind(), expected);
        assert_eq!(error.endpoint(), Some(rig.primary));
        assert_eq!(error.operation(), Some(Op::ReadBattery));
        assert!(error.source().is_some());
        session
            .close_with_topology(&mut rig.topology, reason)
            .unwrap();
        assert_eq!(
            rig.topology
                .capabilities(scope)
                .unwrap()
                .support(Op::ReadBattery),
            Support::Supported
        );
        rig.assert_released();
    }
}
