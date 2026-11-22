//! Script engine behavior is tested without a backend protocol.
use super::*;
use crate::{
    model::{
        connection::EndpointLocator,
        identity::{HardwareIdentity, IdentityAbsence},
        topology::{EndpointSpec, PhysicalDeviceSpec},
    },
    test_support::{
        device::{DeviceBuilder, DevicePart},
        poll::WakeCounter,
    },
};
use std::{sync::atomic::Ordering, task::Wake};

fn fixture(kind: TransportKind) -> crate::test_support::device::DeviceFixture {
    DeviceBuilder::new()
        .physical(DevicePart {
            alias: "unit".into(),
            group: "keyboard".into(),
            interfaces: vec![],
            endpoints: vec!["port".into()],
            components: vec![],
            spec: PhysicalDeviceSpec {
                name: None,
                identity: HardwareIdentity::missing(IdentityAbsence::NotReported),
                usb_ids: None,
                serial: None,
                interfaces: vec![],
                components: vec![],
                endpoints: vec![EndpointSpec {
                    interface: None,
                    transport: kind,
                    locator: EndpointLocator::new("fixture-port").unwrap(),
                    hid: Default::default(),
                }],
            },
        })
        .build()
        .unwrap()
}
fn script(steps: Vec<Step>) -> Script {
    Script {
        steps,
        ..Script::default()
    }
}
fn count(poll: Poll<crate::transport::Result<usize>>) -> usize {
    match poll {
        Poll::Ready(Ok(size)) => size,
        result => panic!("expected transfer, got {result:?}"),
    }
}

#[test]
fn serial_reads_coalesce_ready_chunks_and_preserve_the_unread_remainder() {
    let fixture = fixture(TransportKind::Serial);
    let id = fixture.endpoints["port"];
    let (mut io, controller) = pair(
        id,
        TransportKind::Serial,
        script(vec![
            Step::input(vec![1, 2]).named("a"),
            Step::input(vec![3, 4, 5]).named("b"),
        ]),
    )
    .unwrap();
    let mut cx = Context::from_waker(Waker::noop());
    let mut buffer = [0; 3];
    assert_eq!(
        count(io.poll_io(&mut cx, IoRequest::SerialRead(&mut buffer))),
        3
    );
    assert_eq!(buffer, [1, 2, 3]);
    assert_eq!(
        count(io.poll_io(&mut cx, IoRequest::SerialRead(&mut buffer))),
        2
    );
    assert_eq!(buffer[..2], [4, 5]);
    controller.assert_complete().unwrap();
}

#[test]
fn short_writes_require_the_exact_remaining_suffix_and_do_not_accept_pending_bytes() {
    let fixture = fixture(TransportKind::Serial);
    let id = fixture.endpoints["port"];
    let mut output = Step::output(vec![1, 2, 3]);
    output.action = Action::Output {
        bytes: vec![1, 2, 3],
        accept: VecDeque::from([1, 2]),
    };
    output.after = Some("ready".into());
    let (mut io, control) = pair(
        id,
        TransportKind::Serial,
        Script {
            steps: vec![output],
            gates: BTreeSet::from(["ready".into()]),
            ..Script::default()
        },
    )
    .unwrap();
    let mut cx = Context::from_waker(Waker::noop());
    assert!(
        io.poll_io(&mut cx, IoRequest::SerialWrite(&[1, 2, 3]))
            .is_pending()
    );
    assert!(control.completed_writes().is_empty());
    control.release("ready").unwrap();
    assert_eq!(
        count(io.poll_io(&mut cx, IoRequest::SerialWrite(&[1, 2, 3]))),
        1
    );
    assert_eq!(
        count(io.poll_io(&mut cx, IoRequest::SerialWrite(&[2, 3]))),
        2
    );
    assert_eq!(control.completed_writes(), [vec![1, 2, 3]]);
    control.assert_complete().unwrap();
}

#[test]
fn a_serial_writer_can_offer_a_command_in_several_prefix_buffers() {
    let id = fixture(TransportKind::Serial).endpoints["port"];
    let (mut io, controller) = pair(
        id,
        TransportKind::Serial,
        script(vec![Step::output(vec![1, 2, 3])]),
    )
    .unwrap();
    let mut cx = Context::from_waker(Waker::noop());
    assert_eq!(count(io.poll_io(&mut cx, IoRequest::SerialWrite(&[1]))), 1);
    assert!(controller.completed_writes().is_empty());
    assert_eq!(
        count(io.poll_io(&mut cx, IoRequest::SerialWrite(&[2, 3]))),
        2
    );
    assert_eq!(controller.completed_writes(), [vec![1, 2, 3]]);
    controller.assert_complete().unwrap();
}

#[test]
fn unexpected_output_is_sticky_and_cannot_be_hidden_by_later_correct_input() {
    let id = fixture(TransportKind::Serial).endpoints["port"];
    let (mut io, controller) = pair(
        id,
        TransportKind::Serial,
        script(vec![Step::output(vec![1])]),
    )
    .unwrap();
    let mut cx = Context::from_waker(Waker::noop());
    assert!(matches!(
        io.poll_io(&mut cx, IoRequest::SerialWrite(&[1, 99])),
        Poll::Ready(Err(_))
    ));
    let first = controller.assert_complete().unwrap_err();
    assert!(first.detail.contains("99"));
    assert!(matches!(
        io.poll_io(&mut cx, IoRequest::SerialWrite(&[1])),
        Poll::Ready(Err(_))
    ));
    assert_eq!(controller.assert_complete().unwrap_err(), first);
}

#[test]
fn read_and_write_gates_keep_independent_latest_waiters_and_release_idempotently() {
    let id = fixture(TransportKind::Serial).endpoints["port"];
    let mut input = Step::input(vec![7]);
    input.after = Some("rx".into());
    let mut output = Step::output(vec![8]);
    output.after = Some("tx".into());
    let (mut io, controller) = pair(
        id,
        TransportKind::Serial,
        Script {
            steps: vec![input, output],
            gates: BTreeSet::from(["rx".into(), "tx".into()]),
            ..Script::default()
        },
    )
    .unwrap();
    let read = Arc::new(WakeCounter::default());
    let old_read = Arc::new(WakeCounter::default());
    let write = Arc::new(WakeCounter::default());
    let old_waker = Waker::from(old_read.clone());
    let read_waker = Waker::from(read.clone());
    let write_waker = Waker::from(write.clone());
    let mut buffer = [0];
    assert!(
        io.poll_io(
            &mut Context::from_waker(&old_waker),
            IoRequest::SerialRead(&mut buffer)
        )
        .is_pending()
    );
    assert!(
        io.poll_io(
            &mut Context::from_waker(&read_waker),
            IoRequest::SerialRead(&mut buffer)
        )
        .is_pending()
    );
    assert!(
        io.poll_io(
            &mut Context::from_waker(&write_waker),
            IoRequest::SerialWrite(&[8])
        )
        .is_pending()
    );
    controller.release("tx").unwrap();
    assert_eq!(write.0.load(Ordering::Relaxed), 1);
    assert_eq!(read.0.load(Ordering::Relaxed), 0);
    assert_eq!(
        count(io.poll_io(
            &mut Context::from_waker(&write_waker),
            IoRequest::SerialWrite(&[8])
        )),
        1
    );
    controller.release("rx").unwrap();
    controller.release("rx").unwrap();
    assert_eq!(read.0.load(Ordering::Relaxed), 1);
    assert_eq!(old_read.0.load(Ordering::Relaxed), 0);
    assert_eq!(
        count(io.poll_io(
            &mut Context::from_waker(&read_waker),
            IoRequest::SerialRead(&mut buffer)
        )),
        1
    );
    controller.assert_complete().unwrap();
}

#[test]
fn gate_release_before_poll_and_reentrant_wakes_do_not_lose_progress_or_hold_locks() {
    struct Reenter(Controller);
    impl Wake for Reenter {
        fn wake(self: Arc<Self>) {
            let _ = self.0.transcript();
        }
    }
    let id = fixture(TransportKind::Serial).endpoints["port"];
    let mut input = Step::input(vec![1]);
    input.after = Some("rx".into());
    let (mut io, controller) = pair(
        id,
        TransportKind::Serial,
        Script {
            steps: vec![input],
            gates: BTreeSet::from(["rx".into()]),
            ..Script::default()
        },
    )
    .unwrap();
    let waker = Waker::from(Arc::new(Reenter(controller.clone())));
    let mut buffer = [0];
    assert!(
        io.poll_io(
            &mut Context::from_waker(&waker),
            IoRequest::SerialRead(&mut buffer)
        )
        .is_pending()
    );
    controller.release("rx").unwrap();
    assert_eq!(
        count(io.poll_io(
            &mut Context::from_waker(&waker),
            IoRequest::SerialRead(&mut buffer)
        )),
        1
    );
    let mut input = Step::input(vec![2]);
    input.after = Some("rx".into());
    controller.append(input).unwrap();
    assert_eq!(
        count(io.poll_io(
            &mut Context::from_waker(&waker),
            IoRequest::SerialRead(&mut buffer)
        )),
        1
    );
}

#[test]
fn hid_reports_preserve_transactions_report_ids_and_boundaries() {
    let id = fixture(TransportKind::Hid).endpoints["port"];
    let input = Step {
        label: "input-report".into(),
        op: Operation::HidInput,
        report: None,
        after: None,
        releases: None,
        action: Action::Input {
            bytes: vec![3, 4],
            chunk: None,
        },
    };
    let feature = Step {
        label: "feature".into(),
        op: Operation::HidGetFeature,
        report: Some(7),
        after: None,
        releases: None,
        action: Action::Input {
            bytes: vec![8, 9, 10],
            chunk: None,
        },
    };
    let output = Step {
        label: "output-report".into(),
        op: Operation::HidOutput,
        report: Some(5),
        after: None,
        releases: None,
        action: Action::Output {
            bytes: vec![6],
            accept: VecDeque::new(),
        },
    };
    let (mut io, controller) =
        pair(id, TransportKind::Hid, script(vec![input, feature, output])).unwrap();
    let mut cx = Context::from_waker(Waker::noop());
    let mut buffer = [0; 8];
    assert_eq!(
        count(io.poll_io(&mut cx, IoRequest::HidInput(&mut buffer))),
        2
    );
    assert_eq!(buffer[..2], [3, 4]);
    assert_eq!(
        count(io.poll_io(
            &mut cx,
            IoRequest::HidGetFeature {
                report: 7,
                data: &mut buffer
            }
        )),
        3
    );
    assert_eq!(buffer[..3], [8, 9, 10]);
    assert_eq!(
        count(io.poll_io(
            &mut cx,
            IoRequest::HidOutput {
                report: 5,
                data: &[6]
            }
        )),
        1
    );
    controller.assert_complete().unwrap();
    assert!(controller.set_read_limit(1).is_err());
}

#[test]
fn undersized_hid_buffers_and_wrong_report_ids_fail_instead_of_returning_report_tails() {
    let id = fixture(TransportKind::Hid).endpoints["port"];
    for report in [7, 8] {
        let step = Step {
            label: "feature".into(),
            op: Operation::HidGetFeature,
            report: Some(7),
            after: None,
            releases: None,
            action: Action::Input {
                bytes: vec![1, 2],
                chunk: None,
            },
        };
        let (mut io, controller) = pair(id, TransportKind::Hid, script(vec![step])).unwrap();
        let mut buffer = [0];
        assert!(matches!(
            io.poll_io(
                &mut Context::from_waker(Waker::noop()),
                IoRequest::HidGetFeature {
                    report,
                    data: &mut buffer
                }
            ),
            Poll::Ready(Err(_))
        ));
        assert!(controller.assert_complete().is_err());
    }
}

#[test]
fn explicit_eof_is_persistent_and_terminal_disconnect_is_not_a_one_shot_fault() {
    let id = fixture(TransportKind::Serial).endpoints["port"];
    let eof = Step {
        label: "eof".into(),
        op: Operation::SerialRead,
        report: None,
        after: None,
        releases: None,
        action: Action::Eof,
    };
    let (mut io, controller) = pair(id, TransportKind::Serial, script(vec![eof])).unwrap();
    let mut cx = Context::from_waker(Waker::noop());
    let mut buffer = [0];
    assert_eq!(
        count(io.poll_io(&mut cx, IoRequest::SerialRead(&mut buffer))),
        0
    );
    assert_eq!(
        count(io.poll_io(&mut cx, IoRequest::SerialRead(&mut buffer))),
        0
    );
    controller.assert_complete().unwrap();
    controller.disconnect();
    for _ in 0..2 {
        assert!(matches!(
            io.poll_io(&mut cx, IoRequest::SerialRead(&mut buffer)),
            Poll::Ready(Err(TransportError::Io {
                kind: TransportFailure::Disconnected,
                ..
            }))
        ));
    }
}

#[test]
fn strict_exhaustion_incomplete_scripts_and_cancelled_checkpoints_are_distinct() {
    let mut fixture = fixture(TransportKind::Serial);
    fixture.revalidate().unwrap();
    let id = fixture.endpoints["port"];
    let mut input = Step::input(vec![1]).named("reply");
    input.after = Some("wait".into());
    let controller = Controller::new(
        "cancellation",
        TransportKind::Serial,
        Script {
            steps: vec![input],
            gates: BTreeSet::from(["wait".into()]),
            ..Script::default()
        },
    )
    .unwrap();
    let factory = LeaseFactory::default();
    let controls = BTreeMap::from([(id, controller.clone())]);
    let leases = factory
        .open(
            &fixture.topology,
            &[id],
            &controls,
            AccessGrant::Cooperative,
            64,
        )
        .unwrap();
    assert!(
        factory
            .open(
                &fixture.topology,
                &[id],
                &controls,
                AccessGrant::Cooperative,
                64
            )
            .is_err()
    );
    drop(leases);
    factory.assert_released();
    assert!(controller.assert_complete().is_err());
    controller.assert_cancelled_at("reply").unwrap();
    let (mut io, empty) = pair(id, TransportKind::Serial, Script::default()).unwrap();
    let mut byte = [0];
    assert!(matches!(
        io.poll_io(
            &mut Context::from_waker(Waker::noop()),
            IoRequest::SerialRead(&mut byte)
        ),
        Poll::Ready(Err(_))
    ));
    assert!(empty.assert_complete().is_err());
}

#[test]
fn write_side_failures_pending_tails_and_schema_errors_are_testable_without_a_protocol() {
    let id = fixture(TransportKind::Serial).endpoints["port"];
    let step = Step {
        label: "write-failure".into(),
        op: Operation::SerialWrite,
        report: None,
        after: None,
        releases: None,
        action: Action::Failure {
            kind: TransportFailure::AccessDenied,
            terminal: false,
        },
    };
    let (mut io, controller) = pair(id, TransportKind::Serial, script(vec![step])).unwrap();
    assert!(matches!(
        io.poll_io(
            &mut Context::from_waker(Waker::noop()),
            IoRequest::SerialWrite(&[1])
        ),
        Poll::Ready(Err(TransportError::Io {
            kind: TransportFailure::AccessDenied,
            ..
        }))
    ));
    controller.assert_complete().unwrap();
    let (mut io, _) = pair(
        id,
        TransportKind::Serial,
        Script {
            idle: BTreeSet::from([Operation::SerialRead]),
            ..Script::default()
        },
    )
    .unwrap();
    let mut buffer = [0];
    assert!(
        io.poll_io(
            &mut Context::from_waker(Waker::noop()),
            IoRequest::SerialRead(&mut buffer)
        )
        .is_pending()
    );
    let mut invalid = Step::output(vec![1]);
    invalid.action = Action::Output {
        bytes: vec![1],
        accept: VecDeque::from([2]),
    };
    assert!(Controller::new("bad", TransportKind::Serial, script(vec![invalid])).is_err());
}
