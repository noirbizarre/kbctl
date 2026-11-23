use super::*;
use crate::{
    application::devices::Catalog,
    error::Error,
    model::{input::*, state::Snapshot, time::SystemClock, topology::Topology},
    runtime::Runtime,
    test_support::input::{devices, failure, inventory, scan},
};
use std::{path::Path, sync::Arc};

fn snapshot(scan: Scan) -> Snapshot {
    let mut runtime = Runtime::new(Topology::default(), Arc::new(SystemClock)).unwrap();
    runtime
        .apply_input_scan(scan, MonitorStatus::Watching, runtime.now())
        .unwrap();
    runtime.reader().snapshot().unwrap()
}
fn catalog(scan: Scan) -> Catalog {
    Catalog::new(&snapshot(scan))
}
fn reassigned(catalog: &mut Catalog) {
    let handles = assign(&catalog.devices);
    for (device, handle) in catalog.devices.iter_mut().zip(handles) {
        device.user_selector = handle;
    }
}

#[test]
fn slugs_are_bounded_ascii_handles_with_reserved_generated_and_legacy_namespaces() {
    for (text, expected) in [
        ("  GMMK___Pro (Rev. 1)! ", "gmmk-pro-rev-1"),
        ("Dygma--Defy", "dygma-defy"),
        ("Clavé keyboard", "clav-keyboard"),
    ] {
        assert_eq!(usable(text).as_deref(), Some(expected));
    }
    for text in [
        "",
        " ",
        "!!!",
        "鍵盤",
        "usb-15ac7a64ff726c151efde8b1cbf1218e",
        "attachment-15ac7a64ff726c151efde8b1cbf1218e",
    ] {
        assert_eq!(usable(text), None);
    }
    assert_eq!(usable(&"a".repeat(80)).unwrap().len(), SLUG_LIMIT);
    assert!(
        !usable(&format!("{}-x", "a".repeat(39)))
            .unwrap()
            .ends_with('-')
    );
    assert!(!usable("name--generated--looking").unwrap().contains("--"));
}

#[test]
fn external_models_and_the_internal_keyboard_have_immediately_readable_handles() {
    let catalog = catalog(inventory());
    for (name, id) in [
        ("GMMK Pro", "gmmk-pro"),
        ("Dygma Defy", "dygma-defy"),
        ("AT Translated Set 2 keyboard", "internal-keyboard"),
    ] {
        let device = catalog.devices.iter().find(|d| d.name == name).unwrap();
        assert_eq!(device.user_selector.id, id);
        assert_eq!(
            catalog
                .resolve(Path::new(id), false)
                .unwrap()
                .device
                .group
                .id,
            device.group.id
        );
        assert_eq!(device.user_selector.stability, "inventory_relative");
    }
}

#[test]
fn registry_and_product_bases_do_not_depend_on_the_rendered_display_name() {
    let mut fixture = scan();
    fixture.groups[0].model_label = Some("Reviewed Keyboard Model".into());
    let mut catalog = catalog(fixture);
    assert_eq!(
        catalog.devices[0].user_selector.id,
        "reviewed-keyboard-model"
    );
    catalog.devices[0].name = "Renamed display label".into();
    reassigned(&mut catalog);
    assert_eq!(
        catalog.devices[0].user_selector.id,
        "reviewed-keyboard-model"
    );
    catalog.devices[0].group.descriptor.model_label = None;
    reassigned(&mut catalog);
    assert_eq!(catalog.devices[0].user_selector.id, "synthetic-keyboard");
    assert_eq!(catalog.devices[0].user_selector.name_source, "usb_product");
}

#[test]
fn duplicate_models_have_stable_qualified_aliases_and_ambiguous_bare_handles() {
    let fixture = devices();
    let connected = catalog(fixture.clone());
    let error = connected
        .resolve(Path::new("synthetic-keyboard"), false)
        .err()
        .unwrap();
    assert!(matches!(error, Error::Selection { matches: 2, .. }));
    assert!(
        connected
            .devices
            .iter()
            .all(|d| d.user_selector.bare_ambiguous)
    );
    for device in &connected.devices {
        assert!(device.user_selector.id.starts_with("synthetic-keyboard--"));
        assert!(!device.user_selector.id.contains("--at-"));
        assert_eq!(
            device.user_selector.qualified_stability,
            "model_and_advertised_serial"
        );
        assert!(error.to_string().contains(&device.user_selector.id));
        assert!(error.to_string().contains(&device.name));
        for alias in [
            &device.user_selector.id,
            &device.user_selector.qualified,
            &device.selector.id,
            &device.selector.fallback,
        ] {
            assert_eq!(
                connected
                    .resolve(Path::new(alias), false)
                    .unwrap()
                    .device
                    .group
                    .id,
                device.group.id
            );
        }
    }
    let mut alone = fixture.clone();
    alone.groups.truncate(1);
    let group = alone.groups[0].key.clone();
    alone.endpoints.retain(|e| e.group == group);
    let alone = catalog(alone);
    assert_eq!(alone.devices[0].user_selector.id, "synthetic-keyboard");
    let qualified = &alone.devices[0].user_selector.qualified;
    let same = connected
        .devices
        .iter()
        .find(|d| d.group.descriptor.key == group)
        .unwrap();
    assert_eq!(qualified, &same.user_selector.qualified);
    assert!(connected.resolve(Path::new(qualified), false).is_ok());
    assert!(alone.resolve(Path::new(qualified), false).is_ok());
}

#[test]
fn serial_and_model_evidence_survive_port_moves_event_renumbering_and_inventory_reordering() {
    let fixture = devices();
    let before = catalog(fixture.clone());
    let mut after = fixture;
    for (index, group) in after.groups.iter_mut().enumerate() {
        let previous = group.key.clone();
        group.key = DiscoveryKey(format!("/sys/other-port/{index}").into());
        for (sibling, endpoint) in after
            .endpoints
            .iter_mut()
            .filter(|e| e.group == previous)
            .enumerate()
        {
            endpoint.group = group.key.clone();
            endpoint.key = DiscoveryKey(group.key.0.join(format!(
                "input{}/event{}",
                sibling + 80,
                sibling + 80
            )));
            endpoint.devnode =
                Metadata::Known(format!("/dev/input/event{}", index * 2 + sibling + 80).into());
            endpoint.name = Metadata::Known("Changed kernel label".into());
        }
    }
    after.groups.reverse();
    after.endpoints.reverse();
    let after = catalog(after);
    for old in &before.devices {
        let new = after
            .devices
            .iter()
            .find(|d| d.group.descriptor.serial == old.group.descriptor.serial)
            .unwrap();
        assert_eq!(old.user_selector.id, new.user_selector.id);
        assert_eq!(old.user_selector.qualified, new.user_selector.qualified);
        assert_ne!(old.group.id, new.group.id);
        assert_ne!(old.selector.fallback, new.selector.fallback);
    }
}

#[test]
fn weak_or_duplicate_serials_use_explicit_topology_qualifiers_instead_of_order_counters() {
    for serial in [
        Metadata::Missing,
        Metadata::Known(" ".into()),
        Metadata::Failed(failure()),
        Metadata::Known("duplicate".into()),
    ] {
        let mut fixture = devices();
        for group in &mut fixture.groups {
            group.serial = serial.clone();
        }
        let catalog = catalog(fixture);
        assert_ne!(
            catalog.devices[0].user_selector.id,
            catalog.devices[1].user_selector.id
        );
        for device in &catalog.devices {
            assert!(
                device
                    .user_selector
                    .id
                    .starts_with("synthetic-keyboard--at-")
            );
            assert_eq!(device.user_selector.stability, "attachment_scoped");
            assert!(
                catalog
                    .resolve(Path::new(&device.user_selector.id), false)
                    .is_ok()
            );
        }
    }
}

#[test]
fn normalized_and_truncated_name_collisions_extend_short_evidence_prefixes() {
    for (one, two) in [
        ("Same_model".to_owned(), "Same-model".to_owned()),
        (
            format!("{}first", "a".repeat(40)),
            format!("{}second", "a".repeat(40)),
        ),
    ] {
        let mut fixture = devices();
        fixture.groups[0].product = Metadata::Known(one);
        fixture.groups[1].product = Metadata::Known(two);
        let mut catalog = catalog(fixture);
        catalog.devices[0].selector.id = format!("usb-12345678aaaa{}", "0".repeat(20));
        catalog.devices[1].selector.id = format!("usb-12345678bbbb{}", "0".repeat(20));
        reassigned(&mut catalog);
        assert_eq!(
            catalog.devices[0].user_selector.slug,
            catalog.devices[1].user_selector.slug
        );
        assert!(
            catalog.devices[0]
                .user_selector
                .id
                .ends_with("--12345678aaaa")
        );
        assert!(
            catalog.devices[1]
                .user_selector
                .id
                .ends_with("--12345678bbbb")
        );
    }
}

#[test]
fn residual_full_token_collisions_fail_instead_of_selecting_a_runtime_id_or_first_match() {
    let mut catalog = catalog(devices());
    for device in &mut catalog.devices {
        device.selector.ambiguous = true;
        device.selector.fallback = format!("attachment-{}", "a".repeat(32));
    }
    reassigned(&mut catalog);
    assert!(catalog.devices.iter().all(|d| d.user_selector.ambiguous));
    let query = &catalog.devices[0].user_selector.id;
    assert!(matches!(
        catalog.resolve(Path::new(query), false),
        Err(Error::Selection { matches: 2, .. })
    ));
}

#[test]
fn generated_looking_product_names_and_reserved_legacy_names_do_not_shadow_aliases() {
    let mut fixture = devices();
    let original = catalog(fixture.clone());
    let alias = original.devices[0].user_selector.qualified.clone();
    fixture.groups[1].product = Metadata::Known(alias.clone());
    let with_generated_name = catalog(fixture.clone());
    assert_ne!(with_generated_name.devices[1].user_selector.slug, alias);
    assert_eq!(
        with_generated_name
            .resolve(Path::new(&alias), false)
            .unwrap()
            .device
            .group
            .descriptor
            .key,
        original.devices[0].group.descriptor.key
    );
    let legacy = original.devices[0].selector.id.clone();
    fixture.groups[1].product = Metadata::Known(legacy.clone());
    let with_legacy_name = catalog(fixture);
    assert_eq!(
        with_legacy_name
            .resolve(Path::new(&legacy), false)
            .unwrap()
            .device
            .group
            .descriptor
            .key,
        original.devices[0].group.descriptor.key
    );
}

#[test]
fn missing_names_and_unicode_only_labels_have_honest_generic_fallbacks() {
    let mut fixture = scan();
    fixture.groups[0].product = Metadata::Failed(failure());
    fixture.endpoints[0].name = Metadata::Known("鍵盤 !!!".into());
    let generic = catalog(fixture);
    let selector = &generic.devices[0].user_selector;
    assert!(selector.id.starts_with("keyboard--at-"));
    assert_eq!(selector.name_source, "generic");
    assert!(selector.bare_alias.is_none());
    assert!(generic.resolve(Path::new("keyboard"), false).is_err());
}

#[test]
fn label_fallback_is_deterministic_but_does_not_promise_to_survive_label_changes() {
    let mut fixture = scan();
    fixture.groups[0].product = Metadata::Missing;
    fixture.endpoints[0].name = Metadata::Known("Zebra keyboard".into());
    let mut sibling = fixture.endpoints[0].clone();
    sibling.key.0 = sibling.key.0.with_file_name("event8");
    sibling.name = Metadata::Known("Alpha keyboard".into());
    fixture.endpoints.push(sibling);
    let before = catalog(fixture.clone());
    fixture.endpoints.reverse();
    let reversed = catalog(fixture.clone());
    assert_eq!(before.devices[0].user_selector.id, "alpha-keyboard");
    assert_eq!(
        before.devices[0].user_selector.id,
        reversed.devices[0].user_selector.id
    );
    for endpoint in &mut fixture.endpoints {
        endpoint.name = Metadata::Known("Renamed keyboard".into());
    }
    assert_eq!(
        catalog(fixture).devices[0].user_selector.id,
        "renamed-keyboard"
    );
    assert_eq!(before.devices[0].user_selector.name_source, "kernel_name");
}

#[test]
fn generic_fallbacks_do_not_claim_a_named_models_bare_alias() {
    let mut fixture = devices();
    fixture.groups[0].product = Metadata::Known("Keyboard".into());
    fixture.groups[1].product = Metadata::Missing;
    for endpoint in fixture
        .endpoints
        .iter_mut()
        .filter(|e| e.group == fixture.groups[1].key)
    {
        endpoint.name = Metadata::Missing;
    }
    let catalog = catalog(fixture);
    let named = catalog
        .devices
        .iter()
        .find(|d| d.user_selector.name_source == "usb_product")
        .unwrap();
    let generic = catalog
        .devices
        .iter()
        .find(|d| d.user_selector.name_source == "generic")
        .unwrap();
    assert_eq!(named.user_selector.id, "keyboard");
    assert!(!named.user_selector.bare_ambiguous);
    assert!(generic.user_selector.bare_alias.is_none());
    assert_eq!(
        catalog
            .resolve(Path::new("keyboard"), false)
            .unwrap()
            .device
            .group
            .id,
        named.group.id
    );
}
