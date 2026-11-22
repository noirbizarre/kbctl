use super::*;
use crate::{
    application::Operation,
    model::{time::SystemClock, topology::Topology},
    runtime::Runtime,
    test_support::input::{devices, failure, scan},
};
use std::sync::Arc;

fn document(fixture: Scan, operation: Operation) -> Document {
    let mut runtime = Runtime::new(Topology::default(), Arc::new(SystemClock)).unwrap();
    runtime
        .apply_input_scan(fixture, MonitorStatus::Watching, runtime.now())
        .unwrap();
    Document::view(
        &operation,
        "snapshot",
        &runtime.reader().snapshot().unwrap(),
        &[],
    )
    .unwrap()
}
fn text(document: &Document, detail: bool, width: usize, color: bool) -> String {
    let mut bytes = vec![];
    render(
        &mut bytes,
        document,
        Options {
            detail,
            width,
            color,
        },
    )
    .unwrap();
    String::from_utf8(bytes).unwrap()
}

#[test]
fn ordinary_listing_is_a_handful_of_lines_without_kernel_capability_dumps() {
    let document = document(devices(), Operation::Devices);
    let output = text(&document, false, 140, false);
    assert!(output.starts_with("NAME"));
    assert!(output.lines().count() <= 5);
    assert!(output.contains("Synthetic keyboard"));
    assert!(output.contains("usb-"));
    assert!(output.contains("/dev/input/event"));
    assert!(!output.contains("/sys/") && !output.contains("Capabilities"));
    insta::assert_snapshot!("concise_device_list", output);
}

#[test]
fn inspect_and_verbose_listing_have_named_diagnostic_sections() {
    let fixture = scan();
    let inspect = document(
        fixture.clone(),
        Operation::Inspect("/dev/input/event0".into()),
    );
    let detail = text(&inspect, false, 100, false);
    for heading in [
        "Identity evidence",
        "Grouping and runtime identity",
        "Kernel identifiers",
        "Capabilities",
        "(selected)",
    ] {
        assert!(detail.contains(heading));
    }
    assert!(detail.contains("not checked by metadata discovery"));
    let list = document(fixture, Operation::Devices);
    assert!(text(&list, true, 100, false).contains("Capabilities"));
    assert!(!text(&list, false, 100, false).contains("Capabilities"));
}

#[test]
fn empty_and_failed_discovery_have_distinct_human_messages() {
    for degraded in [false, true] {
        let mut fixture = scan();
        fixture.groups.clear();
        fixture.endpoints.clear();
        if degraded {
            fixture.complete = false;
            fixture.problems.push(failure());
        }
        let output = text(&document(fixture, Operation::Devices), false, 80, false);
        assert!(output.contains(if degraded {
            "discovery is uncertain"
        } else {
            "No input devices found."
        }));
    }
}

#[test]
fn narrow_unicode_output_preserves_copyable_selectors_and_escapes_terminal_controls() {
    let mut fixture = devices();
    fixture.endpoints[0].name =
        Metadata::Known("鍵盤\u{1b}[31m\nlong long long device name".into());
    let document = document(fixture, Operation::Devices);
    let output = text(&document, false, 40, false);
    assert!(output.contains("鍵盤"));
    assert!(!output.contains('\u{1b}'));
    for device in &document.human_view.as_ref().unwrap().devices {
        assert!(output.contains(&device.selector.id));
    }
    let colored = text(&document, false, 40, true);
    assert!(colored.contains('\u{1b}'));
    assert_eq!(console::strip_ansi_codes(&colored), output);
}

#[test]
fn selecting_a_group_without_indicator_candidates_is_not_off_or_unsupported() {
    let mut fixture = scan();
    fixture.endpoints[0].keyboard = KeyboardClassification::Unknown;
    fixture.endpoints[0].capabilities.codes.clear();
    let initial = document(fixture.clone(), Operation::Devices);
    let selector = initial.human_view.unwrap().devices.remove(0).selector.id;
    let output = text(
        &document(
            fixture,
            Operation::Indicators {
                device: Some(selector),
            },
        ),
        false,
        100,
        false,
    );
    assert!(output.contains("not collected"));
    assert!(!output.contains("off") && !output.contains("unsupported"));
}
