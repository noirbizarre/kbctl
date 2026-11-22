//! Backend-specific recipes; reusable assertions contain no toy protocol bytes.
use super::{error::FailureKind, fake::*, *};
use crate::{
    model::{
        capability::{CapabilityOperation as Op, Support},
        capability::{ReportedVersion, VersionKind},
        observation::{
            BatteryState, ChargingState, DeviceObservation, FirmwareInfo, Observation, Percentage,
        },
    },
    test_support::{
        conformance::{self, BackendFactory, Case, EventFactory, ReadFactory, Scenario},
        fixture::{self, Fixture},
        transport::{Controller, Operation, Script, Step, TraceOutcome},
    },
};
use std::collections::{BTreeMap, BTreeSet};

struct Recipe {
    profile: Profile,
}
impl Recipe {
    fn fixture(&self) -> &'static str {
        match self.profile {
            Profile::Battery => "battery",
            Profile::Firmware => "firmware",
            _ => panic!("conformance registration must choose one actual feature adapter"),
        }
    }
    fn bytes(&self) -> Vec<u8> {
        match self.profile {
            Profile::Battery => vec![0x90, 73],
            Profile::Firmware => vec![0x91, b'r', b'4'],
            _ => unreachable!(),
        }
    }
    fn fixture_case(&self, fixture: &Fixture) -> Case {
        fixture
            .require_protocol("kbctl.synthetic.serial", "1")
            .unwrap();
        let mut case = self.case(Scenario::Ready);
        case.channels
            .insert(case.primary, fixture.bind("main").unwrap());
        case.connect_checkpoint = (case.primary, "handshake".into());
        case
    }
}
impl BackendFactory for Recipe {
    fn case(&self, scenario: Scenario) -> Case {
        let mut rig = Rig::new(1, true);
        let mut backend = FakeBackend::new(self.profile, UpdateStrategy::Push);
        match scenario {
            Scenario::Exchange => {
                rig.wires.insert(
                    rig.primary,
                    Wire::scripted(fixture::load(self.fixture()).bind("main").unwrap()),
                );
            }
            Scenario::PendingConnect => {
                let mut handshake = Step::input(frame(&[0x7e])).named("handshake");
                handshake.after = Some("connect-ready".into());
                let control = Controller::new(
                    "pending connection",
                    crate::model::connection::TransportKind::Serial,
                    Script {
                        steps: vec![handshake],
                        gates: BTreeSet::from(["connect-ready".into()]),
                        ..Script::default()
                    },
                )
                .unwrap();
                rig.wires.insert(rig.primary, Wire::scripted(control));
            }
            Scenario::PendingRead | Scenario::Event => rig.wires[&rig.primary].pause_replies(),
            Scenario::Malformed => {
                rig.wires[&rig.primary].pause_replies();
                rig.wires[&rig.primary].push(vec![0xff]);
            }
            Scenario::Disconnect => {
                rig.wires[&rig.primary].pause_replies();
                rig.wires[&rig.primary].fault(crate::transport::TransportFailure::Disconnected)
            }
            Scenario::Unknown => {
                backend.support_override = Some((self.operation(), Support::Unknown))
            }
            Scenario::Unsupported => {
                backend.support_override = Some((self.operation(), Support::Unsupported))
            }
            Scenario::Ready => {}
        }
        let candidate = rig.candidate(&backend);
        Case {
            bounds: Default::default(),
            connect_checkpoint: (
                rig.primary,
                if matches!(scenario, Scenario::PendingConnect | Scenario::Exchange) {
                    "handshake".into()
                } else {
                    "input-0".into()
                },
            ),
            topology: rig.topology,
            backend: Box::new(backend),
            candidate,
            primary: rig.primary,
            endpoints: rig.endpoints.clone(),
            routes: vec![(rig.component.unwrap(), rig.primary)],
            component: rig.component,
            capability_scope: crate::model::capability::CapabilityScope::Component(
                rig.component.unwrap(),
            ),
            clock: rig.clock.clone(),
            channels: rig
                .wires
                .iter()
                .map(|(id, wire)| (*id, wire.control.clone()))
                .collect(),
            leases: rig.factory.clone(),
        }
    }
    fn reporter(&self) -> &str {
        "synthetic.protocol"
    }
    fn expected_support(&self) -> BTreeMap<Op, Support> {
        BTreeMap::from([
            (self.operation(), Support::Supported),
            (Op::SubscribeRuntimeEvents, Support::Supported),
        ])
    }
    fn replacement_channels(
        &self,
        case: &Case,
    ) -> BTreeMap<crate::model::identity::EndpointId, Controller> {
        case.endpoints
            .iter()
            .map(|id| (*id, Wire::new().control))
            .collect()
    }
}
impl ReadFactory for Recipe {
    fn operation(&self) -> Op {
        match self.profile {
            Profile::Battery => Op::ReadBattery,
            Profile::Firmware => Op::ReadFirmware,
            _ => unreachable!(),
        }
    }
    fn execute_before<'a>(
        &'a self,
        session: &'a mut Session,
        case: &Case,
        deadline: Instant,
    ) -> BackendFuture<'a, ObservationDelivery> {
        match self.profile {
            Profile::Battery => session.read_battery(
                case.component.expect("battery recipe requires a component"),
                deadline,
            ),
            Profile::Firmware => session.read_firmware(
                case.component
                    .expect("firmware-read recipe requires a component"),
                deadline,
            ),
            _ => unreachable!(),
        }
    }
    fn expected_read(&self) -> DeviceObservation {
        match self.profile {
            Profile::Battery => DeviceObservation::Battery(BatteryState {
                level: Observation::Known(Percentage::new(73).unwrap()),
                charging: Observation::Known(ChargingState::Discharging),
            }),
            Profile::Firmware => DeviceObservation::Firmware(Observation::Known(
                FirmwareInfo::new(vec![
                    ReportedVersion::new(VersionKind::Firmware, "synthetic/firmware", "r4")
                        .unwrap(),
                ])
                .unwrap(),
            )),
            _ => unreachable!(),
        }
    }
    fn expected_interfaces(&self) -> BTreeSet<Op> {
        BTreeSet::from([self.operation()])
    }
}
impl EventFactory for Recipe {
    fn inject_event(&self, case: &Case) {
        case.channels[&case.primary].input(frame(&[0xa0, 42]));
    }
    fn finish_read(&self, case: &Case) {
        case.channels[&case.primary].input(frame(&self.bytes()));
    }
    fn expected_event(&self) -> DeviceObservation {
        DeviceObservation::Battery(BatteryState {
            level: Observation::Known(Percentage::new(42).unwrap()),
            charging: Observation::Unknown,
        })
    }
}

#[rstest::rstest]
#[case::battery(Profile::Battery)]
#[case::firmware(Profile::Firmware)]
fn each_backend_reuses_lifecycle_and_capability_conformance(#[case] profile: Profile) {
    let recipe = Recipe { profile };
    conformance::lifecycle(&recipe);
    conformance::capabilities(&recipe);
}
#[rstest::rstest]
#[case::battery(Profile::Battery)]
#[case::firmware(Profile::Firmware)]
fn each_backend_reuses_typed_read_and_optional_interface_conformance(#[case] profile: Profile) {
    conformance::reads(&Recipe { profile });
}
#[rstest::rstest]
#[case::battery(Profile::Battery)]
#[case::firmware(Profile::Firmware)]
fn each_backend_reuses_cancellation_deadline_and_failure_conformance(#[case] profile: Profile) {
    let recipe = Recipe { profile };
    conformance::cancellation_and_timeouts(&recipe);
    conformance::failures(&recipe);
}
#[rstest::rstest]
#[case::battery(Profile::Battery)]
#[case::firmware(Profile::Firmware)]
fn each_backend_reuses_async_event_and_reconnect_conformance(#[case] profile: Profile) {
    let recipe = Recipe { profile };
    conformance::events(&recipe);
    conformance::reconnect(&recipe);
}

#[rstest::rstest]
#[case::battery(Profile::Battery)]
#[case::firmware(Profile::Firmware)]
fn versioned_exchanges_survive_fragmented_input_and_partial_command_writes(
    #[case] profile: Profile,
) {
    let recipe = Recipe { profile };
    let fixture = fixture::load(recipe.fixture())
        .fragment("main", 1)
        .unwrap()
        .short_write("main", "read-request", vec![1, 1])
        .unwrap();
    let case = recipe.fixture_case(&fixture);
    let mut session = ready(case.connect()).unwrap();
    ready(session.query_capabilities(case.scope(), case.deadline())).unwrap();
    let result = ready(recipe.execute(&mut session, &case)).unwrap();
    assert_eq!(result.value(), &recipe.expected_read());
    let writes: Vec<_> = case.channels[&case.primary]
        .transcript()
        .into_iter()
        .filter(|trace| trace.op == Some(Operation::SerialWrite))
        .collect();
    assert!(
        writes
            .iter()
            .filter(|trace| trace.outcome == TraceOutcome::Progress(1))
            .count()
            >= 2
    );
    session.shutdown();
    case.assert_complete();
    case.leases.assert_released();
}

#[test]
fn a_checked_in_event_fixture_interleaves_a_gated_reply_without_losing_wakeups() {
    let recipe = Recipe {
        profile: Profile::Firmware,
    };
    let fixture = fixture::load("event");
    let case = recipe.fixture_case(&fixture);
    let mut session = ready(case.connect()).unwrap();
    let mut events = session.take_events().unwrap();
    let counter = Arc::new(WakeCounter::default());
    let waker = std::task::Waker::from(counter.clone());
    let mut request = recipe.execute(&mut session, &case);
    assert!(
        request
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending()
    );
    assert_eq!(
        ready(events.next()).unwrap().unwrap().value(),
        &recipe.expected_event()
    );
    case.channels[&case.primary].release("reply-ready").unwrap();
    assert!(counter.0.load(std::sync::atomic::Ordering::Relaxed) > 0);
    assert_eq!(ready(request).unwrap().value(), &recipe.expected_read());
    session.shutdown();
    case.assert_complete();
    case.leases.assert_released();
}

#[test]
fn complete_malformed_frames_and_truncated_pending_frames_fail_for_different_reasons() {
    let malformed_recipe = Recipe {
        profile: Profile::Battery,
    };
    let malformed = malformed_recipe.fixture_case(&fixture::load("malformed"));
    let mut session = ready(malformed.connect()).unwrap();
    assert_eq!(
        ready(malformed_recipe.execute(&mut session, &malformed))
            .unwrap_err()
            .kind(),
        FailureKind::Protocol
    );
    malformed.assert_complete();
    malformed.leases.assert_released();
    let recipe = Recipe {
        profile: Profile::Firmware,
    };
    let case = recipe.fixture_case(&fixture::load("truncated"));
    let mut session = ready(case.connect()).unwrap();
    let mut request = recipe.execute(&mut session, &case);
    assert!(poll_once(request.as_mut()).is_pending());
    case.clock.advance(Duration::from_secs(11));
    assert_eq!(
        ready(request).unwrap_err().kind(),
        FailureKind::DeadlineExceeded
    );
    case.leases.assert_released();
    let eof = fixture::load("truncated")
        .end_after("main", "read-reply", None)
        .unwrap();
    let case = recipe.fixture_case(&eof);
    let mut session = ready(case.connect()).unwrap();
    let error = ready(recipe.execute(&mut session, &case)).unwrap_err();
    assert_eq!(error.kind(), FailureKind::Protocol);
    assert!(error.to_string().contains("truncated"));
    case.assert_complete();
    case.leases.assert_released();
}

#[test]
fn invalid_frame_lengths_and_zero_progress_output_are_terminal_without_spinning() {
    let recipe = Recipe {
        profile: Profile::Battery,
    };
    for value in [vec![0, 0], vec![0xff, 0xff]] {
        let case = recipe.case(Scenario::Ready);
        let mut session = ready(case.connect()).unwrap();
        case.channels[&case.primary].clear_input();
        case.channels[&case.primary].input(value);
        let error = ready(recipe.execute(&mut session, &case)).unwrap_err();
        assert!(matches!(
            error.kind(),
            FailureKind::Protocol | FailureKind::ResourceLimit
        ));
        case.leases.assert_released();
    }
    let fixture = fixture::load("battery")
        .short_write("main", "read-request", vec![0])
        .unwrap();
    let case = recipe.fixture_case(&fixture);
    let mut session = ready(case.connect()).unwrap();
    ready(session.query_capabilities(case.scope(), case.deadline())).unwrap();
    let error = ready(recipe.execute(&mut session, &case)).unwrap_err();
    assert_eq!(error.kind(), FailureKind::Protocol);
    case.channels[&case.primary]
        .assert_cancelled_at("read-request")
        .unwrap();
    case.leases.assert_released();
}

#[test]
fn a_pending_output_gate_does_not_block_unsolicited_input_or_pretend_the_command_was_sent() {
    let recipe = Recipe {
        profile: Profile::Firmware,
    };
    let mut case = recipe.case(Scenario::Ready);
    let mut command = Step::output(vec![0x11, 0]).named("command");
    command.after = Some("tx-ready".into());
    command.releases = Some("command-sent".into());
    let event = Step::input(frame(&[0xa0, 42])).named("event");
    let mut reply = Step::input(frame(&[0x91, b'r', b'4'])).named("reply");
    reply.after = Some("command-sent".into());
    let control = Controller::new(
        "independent I/O lanes",
        crate::model::connection::TransportKind::Serial,
        Script {
            steps: vec![
                Step::input(frame(&[0x7e])).named("handshake"),
                command,
                event,
                reply,
            ],
            gates: BTreeSet::from(["tx-ready".into(), "command-sent".into()]),
            idle: BTreeSet::from([Operation::SerialRead]),
        },
    )
    .unwrap();
    case.channels.insert(case.primary, control.clone());
    let mut session = ready(case.connect()).unwrap();
    let mut events = session.take_events().unwrap();
    let mut request = recipe.execute(&mut session, &case);
    assert!(poll_once(request.as_mut()).is_pending());
    assert!(control.completed_writes().is_empty());
    assert_eq!(
        ready(events.next()).unwrap().unwrap().value(),
        &recipe.expected_event()
    );
    control.release("tx-ready").unwrap();
    let response = ready(request).unwrap();
    assert_eq!(response.value(), &recipe.expected_read());
    session.shutdown();
    case.assert_complete();
    case.leases.assert_released();
}

#[test]
fn output_side_transport_failures_propagate_through_the_real_session_and_release_ownership() {
    let recipe = Recipe {
        profile: Profile::Battery,
    };
    let case = recipe.case(Scenario::Ready);
    let mut session = ready(case.connect()).unwrap();
    case.channels[&case.primary].fault(
        Operation::SerialWrite,
        crate::transport::TransportFailure::AccessDenied,
        false,
    );
    let error = ready(recipe.execute(&mut session, &case)).unwrap_err();
    assert_eq!(error.kind(), FailureKind::AccessDenied);
    assert_eq!(error.operation(), Some(Op::ReadBattery));
    assert!(case.channels[&case.primary].completed_writes().is_empty());
    case.assert_complete();
    case.leases.assert_released();
}

#[test]
fn lifecycle_and_capability_groups_do_not_require_a_component_or_read_operation_adapter() {
    struct MetadataOnly(Recipe);
    impl BackendFactory for MetadataOnly {
        fn case(&self, scenario: Scenario) -> Case {
            let mut case = self.0.case(scenario);
            case.topology
                .remove_component(case.component.take().unwrap())
                .unwrap();
            case.routes.clear();
            let keyboard = case
                .topology
                .physical_device(case.topology.endpoint(case.primary).unwrap().physical())
                .unwrap()
                .keyboard();
            case.capability_scope = crate::model::capability::CapabilityScope::Keyboard(keyboard);
            let backend = FakeBackend::new(
                Profile::Unknown,
                UpdateStrategy::Polling {
                    interval: Duration::from_secs(1),
                },
            );
            case.candidate = BackendCandidate::new(
                backend.descriptor().id(),
                "unknown",
                case.endpoints.clone(),
                vec![MatchEvidence::Transport(
                    crate::model::connection::TransportKind::Serial,
                )],
            )
            .unwrap();
            case.backend = Box::new(backend);
            case
        }
        fn reporter(&self) -> &str {
            self.0.reporter()
        }
        fn expected_support(&self) -> BTreeMap<Op, Support> {
            BTreeMap::new()
        }
        fn replacement_channels(
            &self,
            case: &Case,
        ) -> BTreeMap<crate::model::identity::EndpointId, Controller> {
            self.0.replacement_channels(case)
        }
    }
    let metadata = MetadataOnly(Recipe {
        profile: Profile::Firmware,
    });
    conformance::lifecycle(&metadata);
    conformance::capabilities(&metadata);
}
