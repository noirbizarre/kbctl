use super::*;
use crate::{
    model::topology::Topology,
    registry::builtin,
    test_support::{
        clock::ManualClock,
        input::{failure, scan},
    },
};
use std::{collections::VecDeque, sync::Arc};

struct Fake {
    scans: VecDeque<Scan>,
    current: Scan,
    log: Vec<&'static str>,
    notifications: VecDeque<Result<Drain, Problem>>,
    starts: VecDeque<Result<(), Problem>>,
}
impl Fake {
    fn new() -> Self {
        Self {
            scans: VecDeque::new(),
            current: scan(),
            log: vec![],
            notifications: VecDeque::new(),
            starts: VecDeque::new(),
        }
    }
}
impl Source for Fake {
    fn start_monitor(&mut self) -> Result<(), Problem> {
        self.log.push("monitor");
        self.starts.pop_front().unwrap_or(Ok(()))
    }
    fn drain(&mut self, _: usize) -> Result<Drain, Problem> {
        self.log.push("drain");
        self.notifications
            .pop_front()
            .unwrap_or(Ok(Drain::default()))
    }
    fn scan(&mut self) -> Scan {
        self.log.push("scan");
        if let Some(scan) = self.scans.pop_front() {
            self.current = scan;
        }
        self.current.clone()
    }
}
fn setup(source: Fake) -> (Service<Fake>, Runtime, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new());
    let runtime = Runtime::new(Topology::new(), clock.clone()).unwrap();
    let service = Service::new(
        source,
        builtin::registry().unwrap(),
        Config::default(),
        clock.time(0),
    )
    .unwrap();
    (service, runtime, clock)
}
#[test]
fn monitoring_starts_before_enumeration_and_periodic_scans_work_without_notifications() {
    let (mut service, mut runtime, clock) = setup(Fake::new());
    service.step(clock.time(0), &mut runtime).unwrap();
    assert_eq!(service.source.log, ["monitor", "drain", "scan"]);
    assert!(
        service
            .step(clock.time(0), &mut runtime)
            .unwrap()
            .publication
            .is_none()
    );
    service.step(clock.at(30), &mut runtime).unwrap();
    assert_eq!(
        service
            .source
            .log
            .iter()
            .filter(|event| **event == "scan")
            .count(),
        2
    );
}
#[test]
fn monitor_failure_still_publishes_inventory_and_recovers_without_restart() {
    let mut source = Fake::new();
    source.starts.push_back(Err(failure()));
    let (mut service, mut runtime, clock) = setup(source);
    let outcome = service.step(clock.time(0), &mut runtime).unwrap();
    assert_eq!(outcome.problems.len(), 1);
    assert_eq!(runtime.topology().keyboards().count(), 0);
    assert!(matches!(service.monitor, MonitorStatus::Degraded(_)));
    service.step(clock.at(1), &mut runtime).unwrap();
    assert_eq!(service.monitor, MonitorStatus::Watching);
}
#[test]
fn delayed_removal_notifications_only_request_a_scan_of_the_current_instance() {
    let (mut service, mut runtime, clock) = setup(Fake::new());
    service.step(clock.time(0), &mut runtime).unwrap();
    service.source.notifications.push_back(Ok(Drain {
        changed: true,
        pending: true,
    }));
    assert!(
        service
            .step(clock.time(0), &mut runtime)
            .unwrap()
            .publication
            .unwrap()
            .commit
            .is_none()
    );
    assert_eq!(service.next_deadline(), clock.time(0));
}
#[test]
fn classification_requires_keyboard_evidence_and_never_infers_current_led_values() {
    let mut caps = scan().endpoints.remove(0).capabilities;
    let classify =
        |caps: &Capabilities| classification::classify(&InputProperties::new(), caps).kind;
    assert_eq!(classify(&caps), InputClass::Keyboard);
    caps.codes
        .insert(1, Metadata::Known([113, 114, 115].into()));
    assert_eq!(classify(&caps), InputClass::Other);
    caps.codes.insert(1, Metadata::Failed(failure()));
    assert_eq!(classify(&caps), InputClass::Unknown);
    assert_eq!(
        classification::classify(
            &InputProperties::from([("ID_INPUT_KEYBOARD", Metadata::Known("1".into()))]),
            &caps
        )
        .kind,
        InputClass::Unknown
    );
}
#[test]
fn bitmap_word_boundaries_are_explicit_and_malformed_data_is_not_empty() {
    assert_eq!(bitmap::parse("1 1", 64).unwrap(), [0, 64].into());
    assert_eq!(bitmap::parse("1 1", 32).unwrap(), [0, 32].into());
    assert!(bitmap::parse("0\n", 64).unwrap().is_empty());
    for text in ["", " ", "10000000000000000", "-1", "0x1", "g"] {
        assert!(bitmap::parse(text, 64).is_err(), "{text}");
    }
    assert!(bitmap::parse("100000000", 32).is_err());
    assert_eq!(bitmap::parse("1 0 0", 64).unwrap(), [128].into());
}

#[test]
fn receive_failure_restarts_monitoring_and_failed_publication_is_retried() {
    let (mut service, mut runtime, clock) = setup(Fake::new());
    service.step(clock.time(0), &mut runtime).unwrap();
    service.source.notifications.push_back(Err(failure()));
    service.step(clock.time(0), &mut runtime).unwrap();
    assert!(matches!(service.monitor, MonitorStatus::Degraded(_)));
    service.step(clock.at(1), &mut runtime).unwrap();
    assert_eq!(
        service
            .source
            .log
            .iter()
            .filter(|event| **event == "monitor")
            .count(),
        2
    );
    runtime.synchronize(clock.at(3)).unwrap();
    service.source.notifications.push_back(Ok(Drain {
        changed: true,
        pending: false,
    }));
    assert!(service.step(clock.time(2), &mut runtime).is_err());
    assert!(service.dirty);
    assert_eq!(service.next_deadline(), clock.time(2));
    assert!(
        service
            .step(clock.time(3), &mut runtime)
            .unwrap()
            .publication
            .is_some()
    );
}

#[test]
fn input_identification_uses_registry_usb_evidence_without_creating_a_route_or_keyboard() {
    let mut source = Fake::new();
    let registry = builtin::registry().unwrap();
    let usb = registry.definitions()[0].selector.usb;
    source.current.groups[0].usb = Metadata::Known(usb);
    let (mut service, mut runtime, clock) = setup(source);
    service.step(clock.time(0), &mut runtime).unwrap();
    let snapshot = runtime.reader().snapshot().unwrap();
    assert!(snapshot.records.values().any(|record| matches!(record.as_ref(), crate::model::state::StateRecord::InputDevice(device) if device.descriptor.model_label.as_deref() == Some(registry.definitions()[0].model.as_str()))));
    assert!(snapshot.records.values().any(|record| matches!(record.as_ref(), crate::model::state::StateRecord::InputDevice(device) if device.descriptor.identification != crate::model::identity::Identification::Unknown)));
    assert_eq!(runtime.topology().physical_devices().count(), 0);
    assert_eq!(runtime.topology().keyboards().count(), 0);
}

#[test]
fn notification_floods_return_control_and_still_reconcile_on_every_due_turn() {
    let mut source = Fake::new();
    source.notifications.extend((0..3).map(|_| {
        Ok(Drain {
            changed: true,
            pending: true,
        })
    }));
    let (mut service, mut runtime, clock) = setup(source);
    for _ in 0..3 {
        service.step(clock.time(0), &mut runtime).unwrap();
    }
    assert_eq!(
        service
            .source
            .log
            .iter()
            .filter(|event| **event == "scan")
            .count(),
        3
    );
    assert_eq!(service.next_deadline(), clock.time(0));
}

#[test]
fn exhausting_a_notification_budget_without_a_decoded_event_still_schedules_another_turn() {
    let (mut service, mut runtime, clock) = setup(Fake::new());
    service.step(clock.time(0), &mut runtime).unwrap();
    service.source.notifications.push_back(Ok(Drain {
        changed: false,
        pending: true,
    }));
    assert!(
        service
            .step(clock.time(0), &mut runtime)
            .unwrap()
            .publication
            .is_some()
    );
    assert_eq!(service.next_deadline(), clock.time(0));
}
