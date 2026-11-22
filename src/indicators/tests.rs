use super::*;
use crate::model::state::{
    FieldQuality, HostState, SourceAvailability, SourceFailure, StateRecord, Validity,
};
use crate::{
    model::{input::MonitorStatus, topology::Topology},
    test_support::{clock::ManualClock, input::scan},
};
use std::{cell::RefCell, collections::VecDeque, rc::Rc, sync::Arc};
struct FakeLease {
    id: InputEndpointId,
    mode: Mode,
    snapshots: Rc<RefCell<VecDeque<Result<Snapshot, AccessError>>>>,
    drains: Rc<RefCell<VecDeque<Drain>>>,
    clock_advance: Option<Arc<ManualClock>>,
    drain_log: Rc<RefCell<Vec<(InputEndpointId, usize)>>>,
}
impl Lease for FakeLease {
    fn mode(&self) -> Mode {
        self.mode
    }
    fn snapshot(&mut self) -> Result<Snapshot, AccessError> {
        if let Some(clock) = &self.clock_advance {
            clock.advance(std::time::Duration::from_secs(2));
        }
        self.snapshots
            .borrow_mut()
            .pop_front()
            .unwrap_or_else(|| Ok(sample(true)))
    }
    fn drain(&mut self, budget: usize) -> Result<Drain, AccessError> {
        self.drain_log.borrow_mut().push((self.id, budget));
        Ok(self.drains.borrow_mut().pop_front().unwrap_or_default())
    }
    fn downgrade(&mut self, reason: Fallback) {
        self.mode = Mode::Snapshots(reason);
    }
}
struct Fake {
    opens: usize,
    fail: bool,
    mode: Mode,
    snapshots: Rc<RefCell<VecDeque<Result<Snapshot, AccessError>>>>,
    drains: Rc<RefCell<VecDeque<Drain>>>,
    clock_advance: Option<Arc<ManualClock>>,
    drain_log: Rc<RefCell<Vec<(InputEndpointId, usize)>>>,
}
impl Opener for Fake {
    type Lease = FakeLease;
    fn open(&mut self, endpoint: &InputEndpoint) -> Result<FakeLease, AccessError> {
        self.opens += 1;
        if self.fail {
            return Err(denied());
        }
        Ok(FakeLease {
            id: endpoint.id,
            mode: self.mode,
            snapshots: self.snapshots.clone(),
            drains: self.drains.clone(),
            clock_advance: self.clock_advance.clone(),
            drain_log: self.drain_log.clone(),
        })
    }
}
fn denied() -> AccessError {
    AccessError {
        path: "/dev/input/synthetic".into(),
        operation: "open indicators",
        kind: ErrorKind::Permission,
        source: Some(io::Error::from_raw_os_error(13)),
    }
}
fn sample(value: bool) -> Snapshot {
    Snapshot {
        caps: HostValue::Known(value),
        num: HostValue::Known(false),
        scroll: HostValue::Unavailable(HostUnavailable::Unsupported),
    }
}
fn setup(mode: Mode) -> (Service<Fake>, Runtime, Arc<ManualClock>, InputEndpointId) {
    let clock = Arc::new(ManualClock::new());
    let mut runtime = Runtime::new(Topology::new(), clock.clone()).unwrap();
    runtime
        .apply_input_scan(scan(), MonitorStatus::Watching, clock.time(0))
        .unwrap();
    let id = runtime.input_endpoints().next().unwrap().id;
    let fake = Fake {
        opens: 0,
        fail: false,
        mode,
        snapshots: Rc::new(RefCell::new(VecDeque::new())),
        drains: Rc::new(RefCell::new(VecDeque::new())),
        clock_advance: None,
        drain_log: Rc::new(RefCell::new(vec![])),
    };
    (
        Service::new(fake, Config::default(), clock.time(0)).unwrap(),
        runtime,
        clock,
        id,
    )
}
fn host(runtime: &Runtime) -> HostState {
    runtime
        .reader()
        .snapshot()
        .unwrap()
        .records
        .values()
        .find_map(|r| {
            if let StateRecord::Host(host) = r.as_ref() {
                Some(host.as_ref().clone())
            } else {
                None
            }
        })
        .unwrap()
}
#[test]
fn initial_state_and_filtered_notifications_publish_authoritative_snapshots() {
    let (mut service, mut runtime, clock, id) = setup(Mode::Events);
    service.step(clock.time(0), &[], &mut runtime).unwrap();
    assert_eq!(host(&runtime).input, Some(id));
    assert!(
        host(&runtime).fields[&HostLock::Caps]
            .last_known
            .as_ref()
            .unwrap()
            .value
    );
    service
        .opener
        .snapshots
        .borrow_mut()
        .push_back(Ok(sample(false)));
    service.opener.drains.borrow_mut().push_back(Drain {
        changed: true,
        ..Drain::default()
    });
    let generation = service.entries[&id].generation;
    service
        .step(
            clock.at(1),
            &[Ready {
                endpoint: id,
                generation,
                closed: false,
            }],
            &mut runtime,
        )
        .unwrap();
    assert!(
        !host(&runtime).fields[&HostLock::Caps]
            .last_known
            .as_ref()
            .unwrap()
            .value
    );
    assert_eq!(
        host(&runtime).fields[&HostLock::Scroll]
            .failure
            .unwrap()
            .cause,
        SourceFailure::HostUnsupported
    );
}

#[test]
fn an_event_gap_stales_the_source_and_recovery_renews_its_generation_before_publishing() {
    let (mut service, mut runtime, clock, id) = setup(Mode::Events);
    service.step(clock.time(0), &[], &mut runtime).unwrap();
    let before = host(&runtime);
    let old = service.entries[&id].generation;
    service.opener.drains.borrow_mut().push_back(Drain {
        gap: true,
        ..Drain::default()
    });
    service
        .step(
            clock.at(1),
            &[Ready {
                endpoint: id,
                generation: old,
                closed: false,
            }],
            &mut runtime,
        )
        .unwrap();
    assert_eq!(
        host(&runtime).availability,
        SourceAvailability::Unavailable(SourceFailure::EventGap)
    );
    assert_eq!(service.next_deadline(), Some(clock.time(2)));
    service.opener.drains.borrow_mut().push_back(Drain {
        synchronized: true,
        pending: true,
        changed: true,
        ..Drain::default()
    });
    service
        .opener
        .snapshots
        .borrow_mut()
        .push_back(Ok(sample(false)));
    service
        .step(
            clock.time(1),
            &[Ready {
                endpoint: id,
                generation: old,
                closed: false,
            }],
            &mut runtime,
        )
        .unwrap();
    assert_ne!(host(&runtime).generation, before.generation);
    assert_eq!(host(&runtime).availability, SourceAvailability::Active);
    assert_eq!(
        service.next_deadline(),
        Some(clock.time(1)),
        "queued kernel work remains pending after recovery"
    );
    service.step(clock.time(1), &[], &mut runtime).unwrap();
    service.opener.drains.borrow_mut().push_back(Drain {
        changed: true,
        ..Drain::default()
    });
    service
        .step(
            clock.time(1),
            &[Ready {
                endpoint: id,
                generation: old,
                closed: true,
            }],
            &mut runtime,
        )
        .unwrap();
    assert_eq!(
        service.opener.drains.borrow().len(),
        1,
        "old readiness must not touch the renewed lease"
    );
    assert_eq!(host(&runtime).availability, SourceAvailability::Active);
}

#[test]
fn missing_a_recovery_boundary_switches_to_snapshots_without_waiting_forever() {
    let (mut service, mut runtime, clock, id) = setup(Mode::Events);
    service.step(clock.time(0), &[], &mut runtime).unwrap();
    let token = Ready {
        endpoint: id,
        generation: service.entries[&id].generation,
        closed: false,
    };
    service.opener.drains.borrow_mut().push_back(Drain {
        gap: true,
        ..Drain::default()
    });
    service.step(clock.at(1), &[token], &mut runtime).unwrap();
    service.step(clock.at(2), &[], &mut runtime).unwrap();
    assert_eq!(
        service.entries[&id].lease.as_ref().unwrap().mode(),
        Mode::Snapshots(Fallback::RecoveryTimeout)
    );
    assert_eq!(service.next_deadline(), Some(clock.time(3)));
    assert_eq!(host(&runtime).availability, SourceAvailability::Active);
}

#[test]
fn closure_clears_pending_stream_work_and_respects_retry_backoff() {
    let (mut service, mut runtime, clock, id) = setup(Mode::Events);
    service.step(clock.time(0), &[], &mut runtime).unwrap();
    service.entries.get_mut(&id).unwrap().drain_pending = true;
    let token = Ready {
        endpoint: id,
        generation: service.entries[&id].generation,
        closed: true,
    };
    let outcome = service.step(clock.at(1), &[token], &mut runtime).unwrap();
    assert_eq!(outcome.problems.len(), 1);
    assert_eq!(service.next_deadline(), Some(clock.time(2)));
    assert_eq!(service.opener.opens, 1);
    service.step(clock.time(1), &[], &mut runtime).unwrap();
    assert_eq!(service.opener.opens, 1);
    service.step(clock.at(2), &[], &mut runtime).unwrap();
    assert_eq!(service.opener.opens, 2);
}

#[test]
fn independent_endpoints_disagree_without_a_winner_and_budgets_do_not_starve_them() {
    let (mut service, mut runtime, clock, _) = setup(Mode::Events);
    let mut two = scan();
    let mut other = two.endpoints[0].clone();
    other.key.0 = "/sys/devices/synthetic-usb/input1/event1".into();
    other.incarnation.as_mut().unwrap().inode = 100;
    two.endpoints.push(other);
    runtime
        .apply_input_scan(two, MonitorStatus::Watching, clock.time(0))
        .unwrap();
    service.config.endpoints = 1;
    service
        .opener
        .snapshots
        .borrow_mut()
        .extend([Ok(sample(true)), Ok(sample(false))]);
    service.step(clock.time(0), &[], &mut runtime).unwrap();
    assert_eq!(service.opener.opens, 1);
    assert_eq!(service.next_deadline(), Some(clock.time(0)));
    service.step(clock.time(0), &[], &mut runtime).unwrap();
    assert_eq!(service.opener.opens, 2);
    let snapshot = runtime.reader().snapshot().unwrap();
    let hosts: Vec<_> = snapshot
        .records
        .values()
        .filter_map(|record| {
            if let StateRecord::Host(host) = record.as_ref() {
                Some(host)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(hosts.len(), 2);
    assert_ne!(hosts[0].scope, hosts[1].scope);
    assert_ne!(
        hosts[0].fields[&HostLock::Caps]
            .last_known
            .as_ref()
            .unwrap()
            .value,
        hosts[1].fields[&HostLock::Caps]
            .last_known
            .as_ref()
            .unwrap()
            .value
    );
    assert!(!snapshot.records.values().any(|record| matches!(
        record.as_ref(),
        StateRecord::Device(_) | StateRecord::Keyboard { .. }
    )));
    service.stop(&mut runtime).unwrap();
    assert!(service.entries.is_empty());
    let opened = service.opener.opens;
    service.step(clock.time(0), &[], &mut runtime).unwrap();
    assert!(service.entries.is_empty());
    assert_eq!(service.opener.opens, opened);
}

#[test]
fn receipts_are_stamped_after_io_and_publication_retry_does_not_extend_their_deadline() {
    let (mut service, mut runtime, clock, id) = setup(Mode::Snapshots(Fallback::Kernel));
    service.opener.clock_advance = Some(clock.clone());
    service.step(clock.time(0), &[], &mut runtime).unwrap();
    let report = host(&runtime).fields[&HostLock::Caps]
        .report
        .clone()
        .unwrap();
    assert_eq!(
        report.stamp.received_at,
        crate::model::time::Timestamp(std::time::Duration::from_secs(2))
    );
    let mut entry = service.entries.remove(&id).unwrap();
    entry.pending = Some(Receipt {
        value: sample(false),
        sequence: 2,
        received: clock.time(3),
        until: clock.time(6),
    });
    runtime.synchronize(clock.at(5)).unwrap();
    clock.at(4);
    let mut outcome = Outcome {
        publications: vec![],
        problems: vec![],
    };
    assert!(
        service
            .publish(&mut entry, &mut runtime, &mut outcome)
            .is_err()
    );
    assert!(entry.pending.is_some());
    clock.at(7);
    service
        .publish(&mut entry, &mut runtime, &mut outcome)
        .unwrap();
    let report = host(&runtime).fields[&HostLock::Caps]
        .report
        .clone()
        .unwrap();
    assert_eq!(report.stamp.sequence, 2);
    assert_eq!(
        report.stamp.fresh_until,
        crate::model::time::Timestamp(std::time::Duration::from_secs(6))
    );
    assert_eq!(
        host(&runtime).fields[&HostLock::Scroll].failure.unwrap().at,
        crate::model::time::Timestamp(std::time::Duration::from_secs(3))
    );
    assert!(matches!(report.validity, Validity::Stale { .. }));
    assert!(entry.pending.is_none());
}

#[test]
fn idle_event_sources_refresh_slowly_and_expiry_is_driven_by_the_runtime() {
    let (mut service, mut runtime, clock, _) = setup(Mode::Events);
    service.step(clock.time(0), &[], &mut runtime).unwrap();
    assert_eq!(service.next_deadline(), Some(clock.time(30)));
    runtime.advance_time(clock.at(90)).unwrap();
    assert_eq!(
        host(&runtime).fields[&HostLock::Caps].quality(),
        FieldQuality::Stale
    );
    service.step(clock.time(90), &[], &mut runtime).unwrap();
    assert_eq!(
        host(&runtime).fields[&HostLock::Caps].quality(),
        FieldQuality::Fresh
    );
    assert_eq!(service.next_deadline(), Some(clock.time(120)));
}

#[test]
fn discovery_uncertainty_does_not_retire_a_still_bound_indicator_source() {
    let (mut service, mut runtime, clock, id) = setup(Mode::Events);
    service.step(clock.time(0), &[], &mut runtime).unwrap();
    let before = host(&runtime);
    let mut incomplete = scan();
    incomplete.complete = false;
    incomplete.endpoints.clear();
    incomplete.groups.clear();
    incomplete
        .problems
        .push(crate::test_support::input::failure());
    runtime
        .apply_input_scan(incomplete, MonitorStatus::Watching, clock.time(0))
        .unwrap();
    service.step(clock.time(0), &[], &mut runtime).unwrap();
    assert_eq!(host(&runtime), before);
    assert!(service.entries.contains_key(&id));
    assert_eq!(service.opener.opens, 1);
}

#[test]
fn a_hot_first_endpoint_cannot_monopolize_the_global_notification_budget() {
    let (mut service, mut runtime, clock, _) = setup(Mode::Events);
    let mut two = scan();
    let mut other = two.endpoints[0].clone();
    other.key.0 = "/sys/devices/synthetic-usb/input2/event2".into();
    other.incarnation.as_mut().unwrap().inode = 101;
    two.endpoints.push(other);
    runtime
        .apply_input_scan(two, MonitorStatus::Watching, clock.time(0))
        .unwrap();
    service.config.events = 1;
    service.step(clock.time(0), &[], &mut runtime).unwrap();
    let ready: Vec<_> = service
        .entries
        .iter()
        .map(|(id, e)| Ready {
            endpoint: *id,
            generation: e.generation,
            closed: false,
        })
        .collect();
    service
        .opener
        .drains
        .borrow_mut()
        .extend((0..2).map(|_| Drain {
            processed: 1,
            pending: true,
            ..Drain::default()
        }));
    service.step(clock.at(1), &ready, &mut runtime).unwrap();
    service.step(clock.time(1), &[], &mut runtime).unwrap();
    let log = service.opener.drain_log.borrow();
    assert_eq!(log.len(), 2);
    assert_ne!(log[0].0, log[1].0);
    assert_eq!((log[0].1, log[1].1), (1, 1));
}
#[test]
fn snapshot_fallback_uses_its_deadline_and_permission_recovery_renews_the_source() {
    let (mut service, mut runtime, clock, _) = setup(Mode::Snapshots(Fallback::Kernel));
    service.opener.fail = true;
    service.step(clock.time(0), &[], &mut runtime).unwrap();
    let unavailable = host(&runtime);
    assert_eq!(
        unavailable.availability,
        SourceAvailability::Unavailable(SourceFailure::PermissionDenied)
    );
    service.opener.fail = false;
    service.step(clock.at(1), &[], &mut runtime).unwrap();
    let current = host(&runtime);
    assert_eq!(current.availability, SourceAvailability::Active);
    assert_ne!(current.generation, unavailable.generation);
    assert_eq!(service.next_deadline(), Some(clock.time(2)));
}
#[test]
fn input_retirement_invalidates_host_state_before_the_collector_closes_and_retires_it() {
    let (mut service, mut runtime, clock, _) = setup(Mode::Events);
    service.step(clock.time(0), &[], &mut runtime).unwrap();
    let mut empty = scan();
    empty.groups.clear();
    empty.endpoints.clear();
    runtime
        .apply_input_scan(empty, MonitorStatus::Watching, clock.time(0))
        .unwrap();
    assert_eq!(
        host(&runtime).availability,
        SourceAvailability::Unavailable(SourceFailure::InvalidBinding)
    );
    service.step(clock.time(0), &[], &mut runtime).unwrap();
    assert!(service.entries.is_empty());
    assert!(
        !runtime
            .reader()
            .snapshot()
            .unwrap()
            .records
            .values()
            .any(|r| matches!(r.as_ref(), StateRecord::Host(_)))
    );
}
