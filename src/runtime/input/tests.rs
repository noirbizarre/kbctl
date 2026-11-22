use super::*;
use crate::test_support::{
    clock::ManualClock,
    input::{failure, scan},
};

fn setup() -> (Runtime, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new());
    (Runtime::new(Topology::new(), clock.clone()).unwrap(), clock)
}
fn inventory(runtime: &Runtime) -> Vec<Arc<StateRecord>> {
    runtime
        .reader()
        .snapshot()
        .unwrap()
        .records
        .into_values()
        .filter(|record| {
            matches!(
                record.as_ref(),
                StateRecord::InputDevice(_) | StateRecord::InputEndpoint(_)
            )
        })
        .collect()
}
#[test]
fn discovery_does_not_create_keyboards_or_host_indicator_values_and_unchanged_scans_do_not_commit()
{
    let (mut runtime, clock) = setup();
    runtime
        .apply_input_scan(scan(), MonitorStatus::Watching, clock.time(0))
        .unwrap();
    assert_eq!(runtime.topology().keyboards().count(), 0);
    assert_eq!(inventory(&runtime).len(), 2);
    assert!(
        runtime
            .apply_input_scan(scan(), MonitorStatus::Watching, clock.time(0))
            .unwrap()
            .commit
            .is_none()
    );
    assert!(
        !runtime
            .reader()
            .snapshot()
            .unwrap()
            .records
            .values()
            .any(|record| matches!(
                record.as_ref(),
                StateRecord::Host(_) | StateRecord::Device(_)
            ))
    );
}
#[test]
fn failed_enumeration_retains_membership_but_a_complete_empty_scan_removes_it() {
    let (mut runtime, clock) = setup();
    runtime
        .apply_input_scan(scan(), MonitorStatus::Watching, clock.time(0))
        .unwrap();
    let before = inventory(&runtime);
    let empty = Scan {
        groups: vec![],
        endpoints: vec![],
        complete: false,
        problems: vec![failure()],
    };
    runtime
        .apply_input_scan(empty.clone(), MonitorStatus::Watching, clock.time(0))
        .unwrap();
    assert_eq!(inventory(&runtime), before);
    runtime
        .apply_input_scan(
            Scan {
                complete: true,
                problems: vec![],
                ..empty
            },
            MonitorStatus::Watching,
            clock.time(0),
        )
        .unwrap();
    assert!(inventory(&runtime).is_empty());
}
#[test]
fn replacement_at_the_same_locator_gets_new_ids_without_removing_its_sibling_group() {
    let (mut runtime, clock) = setup();
    runtime
        .apply_input_scan(scan(), MonitorStatus::Watching, clock.time(0))
        .unwrap();
    let before = inventory(&runtime);
    let mut replacement = scan();
    replacement.endpoints[0].incarnation.as_mut().unwrap().inode = 99;
    let commit = runtime
        .apply_input_scan(replacement, MonitorStatus::Watching, clock.time(0))
        .unwrap()
        .commit
        .unwrap();
    assert_eq!(commit.removals.len(), 1);
    assert!(
        before
            .iter()
            .filter(|record| matches!(record.as_ref(), StateRecord::InputDevice(_)))
            .all(|record| inventory(&runtime).contains(record))
    );
}
#[test]
fn invalid_scans_and_future_timestamps_leave_the_published_inventory_untouched() {
    let (mut runtime, clock) = setup();
    let before = runtime.reader().snapshot().unwrap();
    let mut invalid = scan();
    invalid.endpoints.push(invalid.endpoints[0].clone());
    assert!(
        runtime
            .apply_input_scan(invalid, MonitorStatus::Watching, clock.time(0))
            .is_err()
    );
    assert_eq!(runtime.reader().snapshot().unwrap(), before);
    assert!(
        runtime
            .apply_input_scan(scan(), MonitorStatus::Watching, clock.time(1))
            .is_err()
    );
    assert_eq!(runtime.reader().snapshot().unwrap(), before);
}

#[test]
fn removing_a_sibling_preserves_the_group_and_only_retires_that_endpoint() {
    let (mut runtime, clock) = setup();
    let mut two = scan();
    let mut sibling = two.endpoints[0].clone();
    sibling.key.0 = "/sys/devices/synthetic-usb/input1/event1".into();
    sibling.incarnation.as_mut().unwrap().inode = 3;
    two.endpoints.push(sibling);
    runtime
        .apply_input_scan(two, MonitorStatus::Watching, clock.time(0))
        .unwrap();
    let before = inventory(&runtime);
    let commit = runtime
        .apply_input_scan(scan(), MonitorStatus::Watching, clock.time(0))
        .unwrap()
        .commit
        .unwrap();
    assert_eq!(commit.removals.len(), 1);
    assert!(commit.upserts.is_empty());
    assert!(
        inventory(&runtime)
            .iter()
            .all(|record| before.contains(record))
    );
}

#[test]
fn retiring_a_physical_unit_clears_only_its_explicit_input_association_in_the_same_commit() {
    let (mut runtime, clock) = setup();
    let (physical, _) = runtime
        .add_physical_device(
            None,
            PhysicalDeviceSpec {
                name: None,
                identity: model::identity::HardwareIdentity::missing(
                    model::identity::IdentityAbsence::NotReported,
                ),
                usb_ids: None,
                serial: None,
                interfaces: vec![],
                endpoints: vec![],
                components: vec![],
            },
            clock.time(0),
        )
        .unwrap();
    runtime
        .apply_input_scan(scan(), MonitorStatus::Watching, clock.time(0))
        .unwrap();
    let group = runtime.inputs.groups.values().next().unwrap().id;
    runtime
        .associate_input_device(group, Some(physical), clock.time(0))
        .unwrap();
    assert_eq!(
        runtime.inputs.groups.values().next().unwrap().physical,
        Some(physical)
    );
    let (snapshot, mut subscription) = runtime.reader().subscribe_and_snapshot(4).unwrap();
    let commit = runtime
        .retire_physical_device(physical, clock.time(0))
        .unwrap()
        .commit
        .unwrap();
    assert!(
        snapshot
            .records
            .contains_key(&RecordKey::Physical(physical))
    );
    let StateRecord::InputDevice(updated) = commit.upserts[&RecordKey::InputDevice(group)].as_ref()
    else {
        panic!("updated input group");
    };
    assert_eq!(updated.physical, None);
    assert!(commit.removals.contains(&RecordKey::Physical(physical)));
    assert_eq!(inventory(&runtime).len(), 2);
    assert!(
        subscription
            .poll_next(&mut std::task::Context::from_waker(std::task::Waker::noop()))
            .is_ready()
    );
}

#[test]
fn an_epoch_reservation_failure_leaves_input_state_and_snapshot_unchanged() {
    struct FailOnRollover(bool);
    impl EpochSource for FailOnRollover {
        fn next(&mut self) -> std::result::Result<Epoch, StateError> {
            if self.0 {
                return Err(StateError::Epoch(std::io::Error::other(
                    "synthetic entropy failure",
                )));
            }
            self.0 = true;
            Ok(Epoch::from_bytes([7; 16]))
        }
    }
    let clock = Arc::new(ManualClock::new());
    let mut runtime = Runtime::with_epochs(
        Topology::new(),
        clock.clone(),
        Box::new(FailOnRollover(false)),
    )
    .unwrap();
    runtime.store.force_revision(u64::MAX);
    let before = runtime.reader().snapshot().unwrap();
    assert!(
        runtime
            .apply_input_scan(scan(), MonitorStatus::Watching, clock.time(0))
            .is_err()
    );
    assert_eq!(runtime.reader().snapshot().unwrap(), before);
    assert!(runtime.inputs.groups.is_empty());
    assert!(!runtime.inputs.started);
}

#[test]
fn an_uncertain_instance_is_renewed_after_monitoring_coverage_is_lost() {
    let (mut runtime, clock) = setup();
    let mut uncertain = scan();
    uncertain.groups[0].incarnation = None;
    uncertain.endpoints[0].incarnation = None;
    runtime
        .apply_input_scan(uncertain.clone(), MonitorStatus::Watching, clock.time(0))
        .unwrap();
    let before = inventory(&runtime);
    runtime
        .apply_input_scan(uncertain, MonitorStatus::Degraded(failure()), clock.time(0))
        .unwrap();
    assert!(
        inventory(&runtime)
            .iter()
            .all(|record| !before.contains(record))
    );
}

#[test]
fn unreadable_grouping_metadata_does_not_reparent_a_confirmed_live_endpoint() {
    let (mut runtime, clock) = setup();
    runtime
        .apply_input_scan(scan(), MonitorStatus::Watching, clock.time(0))
        .unwrap();
    let group = runtime.inputs.groups.values().next().unwrap().id;
    let endpoint = runtime.inputs.endpoints.values().next().unwrap().id;
    let mut uncertain = scan();
    uncertain.groups[0].key.0 = "/sys/fallback-input-parent".into();
    uncertain.endpoints[0].group = uncertain.groups[0].key.clone();
    uncertain.endpoints[0].group_confirmed = false;
    uncertain.endpoints[0].identity.bus = Metadata::Failed(failure());
    uncertain.groups[0].usb = Metadata::Missing;
    uncertain.problems.push(failure());
    let mut invalid = uncertain.clone();
    invalid.groups.push(invalid.groups[0].clone());
    let before = runtime.reader().snapshot().unwrap();
    assert!(
        runtime
            .apply_input_scan(invalid, MonitorStatus::Watching, clock.time(0))
            .is_err()
    );
    assert_eq!(runtime.reader().snapshot().unwrap(), before);
    runtime
        .apply_input_scan(uncertain, MonitorStatus::Watching, clock.time(0))
        .unwrap();
    assert_eq!(runtime.inputs.groups.values().next().unwrap().id, group);
    assert_eq!(
        runtime.inputs.endpoints.values().next().unwrap().id,
        endpoint
    );
    assert_eq!(runtime.inputs.groups.len(), 1);
    runtime
        .apply_input_scan(scan(), MonitorStatus::Watching, clock.time(0))
        .unwrap();
    assert_eq!(runtime.inputs.groups.values().next().unwrap().id, group);
    assert_eq!(
        runtime.inputs.endpoints.values().next().unwrap().id,
        endpoint
    );
}
