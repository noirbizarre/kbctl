use super::*;
use crate::{
    model::{time::SystemClock, topology::Topology},
    runtime::Runtime,
    test_support::input::{devices, failure, scan},
};
use std::sync::Arc;

pub(crate) fn snapshot(scan: Scan) -> Snapshot {
    let mut runtime = Runtime::new(Topology::default(), Arc::new(SystemClock)).unwrap();
    runtime
        .apply_input_scan(scan, MonitorStatus::Watching, runtime.now())
        .unwrap();
    runtime.reader().snapshot().unwrap()
}

#[test]
fn advertised_selectors_survive_reconnect_renumbering_and_independent_runtimes() {
    let original = devices();
    let first = Catalog::new(&snapshot(original.clone()));
    let mut renumbered = original;
    for (index, endpoint) in renumbered.endpoints.iter_mut().enumerate() {
        endpoint.key.0 = endpoint
            .key
            .0
            .with_file_name(format!("event{}", index + 99));
        endpoint.devnode = Metadata::Known(format!("/dev/input/event{}", index + 99).into());
        endpoint.name = Metadata::Known("A different label".into());
        endpoint.incarnation.as_mut().unwrap().inode += 100;
    }
    // A USB port move changes the whole attachment locator, not just eventN.
    for (index, group) in renumbered.groups.iter_mut().enumerate() {
        let before = group.key.clone();
        group.key = DiscoveryKey(format!("/sys/other-port/unit{index}").into());
        for (sibling, endpoint) in renumbered
            .endpoints
            .iter_mut()
            .filter(|e| e.group == before)
            .enumerate()
        {
            endpoint.group = group.key.clone();
            endpoint.key.0 = group.key.0.join(format!(
                "input{}/event{}",
                index * 2 + sibling + 99,
                index * 2 + sibling + 99
            ));
        }
    }
    let second = Catalog::new(&snapshot(renumbered));
    for (left, right) in first.devices.iter().zip(&second.devices) {
        assert_eq!(left.selector.id, right.selector.id);
        assert_ne!(left.selector.fallback, right.selector.fallback);
        assert_ne!(left.group.id, right.group.id);
        assert_eq!(right.selector.verification, "advertised");
        assert_eq!(
            second
                .resolve(Path::new(&left.selector.id), false)
                .unwrap()
                .device
                .group
                .id,
            right.group.id
        );
    }
}

#[test]
fn selectors_preserve_serial_bytes_and_namespace_usb_identity() {
    let mut fixture = scan();
    fixture.groups[0].serial = Metadata::Known("Case sensitive ".into());
    let original = Catalog::new(&snapshot(fixture.clone()))
        .devices
        .remove(0)
        .selector
        .id;
    for serial in ["case sensitive ", "Case sensitive", " Case sensitive "] {
        fixture.groups[0].serial = Metadata::Known(serial.into());
        assert_ne!(
            original,
            Catalog::new(&snapshot(fixture.clone())).devices[0]
                .selector
                .id
        );
    }
    fixture.groups[0].serial = Metadata::Known("Case sensitive ".into());
    if let Metadata::Known(usb) = &mut fixture.groups[0].usb {
        usb.product += 1;
    }
    assert_ne!(
        original,
        Catalog::new(&snapshot(fixture)).devices[0].selector.id
    );
}

#[test]
fn insufficient_identity_gets_an_explicit_attachment_fallback() {
    for serial in [
        Metadata::Missing,
        Metadata::Known("  ".into()),
        Metadata::Failed(failure()),
    ] {
        let mut fixture = scan();
        fixture.groups[0].serial = serial;
        let catalog = Catalog::new(&snapshot(fixture));
        let device = &catalog.devices[0];
        assert_eq!(device.selector.kind, "attachment");
        assert_eq!(device.selector.verification, "unverified");
        assert_eq!(device.selector.id, device.selector.fallback);
        assert!(
            catalog
                .resolve(Path::new(&device.selector.id), false)
                .is_ok()
        );
        assert!(
            catalog
                .resolve(Path::new("/dev/input/event0"), false)
                .is_err()
        );
        assert!(
            catalog
                .resolve(Path::new("/dev/input/event0"), true)
                .is_ok()
        );
    }
}

#[test]
fn duplicate_advertised_identity_is_ambiguous_without_merging_attachments() {
    let mut fixture = devices();
    fixture.groups[1].serial = fixture.groups[0].serial.clone();
    let catalog = Catalog::new(&snapshot(fixture));
    assert_eq!(catalog.devices.len(), 2);
    assert!(catalog.devices.iter().all(|d| d.selector.ambiguous));
    let selector = &catalog.devices[0].selector.id;
    let error = catalog.resolve(Path::new(selector), false).err().unwrap();
    assert!(matches!(&error, Error::Selection { matches: 2, .. }));
    for device in &catalog.devices {
        assert!(error.to_string().contains(&device.selector.fallback));
        assert_eq!(
            catalog
                .resolve(Path::new(&device.selector.fallback), false)
                .unwrap()
                .device
                .group
                .id,
            device.group.id
        );
    }
}

#[test]
fn shortened_digest_collisions_cannot_select_the_first_device() {
    let catalog = Catalog::with_digest(&snapshot(devices()), |_| "forced-collision".into());
    for device in &catalog.devices {
        assert!(device.selector.ambiguous && device.selector.fallback_ambiguous);
        for selector in [&device.selector.id, &device.selector.fallback] {
            assert!(matches!(
                catalog.resolve(Path::new(selector), false),
                Err(Error::Selection { matches: 2, .. })
            ));
        }
    }
}

#[test]
fn aggregation_never_turns_disagreement_or_missing_evidence_into_off() {
    let on = LockObservation {
        state: LockState::Known,
        value: Some(true),
    };
    let off = LockObservation {
        state: LockState::Known,
        value: Some(false),
    };
    assert_eq!(LockSummary::aggregate([on, on].into_iter()).observation, on);
    assert_eq!(
        LockSummary::aggregate([off, off].into_iter()).observation,
        off
    );
    assert_eq!(
        LockSummary::aggregate([on, off].into_iter())
            .observation
            .state,
        LockState::Disagreement
    );
    for state in [
        LockState::Unknown,
        LockState::Unsupported,
        LockState::Stale,
        LockState::PermissionDenied,
        LockState::Unavailable,
        LockState::Error,
        LockState::NotCollected,
    ] {
        let uncertain = LockObservation { state, value: None };
        assert_eq!(
            LockSummary::aggregate([uncertain, uncertain].into_iter())
                .observation
                .state,
            state
        );
        for known in [on, off] {
            let summary = LockSummary::aggregate([known, uncertain].into_iter());
            assert_eq!(summary.observation.state, LockState::Mixed);
            assert!(summary.observation.value.is_none());
        }
    }
    assert_eq!(
        LockSummary::aggregate(std::iter::empty()).observation.state,
        LockState::NotCollected
    );
}

#[test]
fn selector_encoding_has_a_fixed_version_one_test_vector() {
    let mut fixture = scan();
    fixture.groups[0].serial = Metadata::Known("unit-0".into());
    let selector = Catalog::new(&snapshot(fixture)).devices.remove(0).selector;
    // Independent SHA-256 computation guards the public identity encoding against accidental refactors.
    assert_eq!(selector.id, "usb-15ac7a64ff726c151efde8b1cbf1218e");
}
