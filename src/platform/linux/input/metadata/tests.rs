use super::*;
use std::cell::RefCell;

struct Synthetic {
    reads: RefCell<Vec<PathBuf>>,
    denied: Option<&'static str>,
    replace: bool,
    inspections: RefCell<usize>,
    roles: InputProperties,
}
impl Reader for Synthetic {
    fn entries(&self, _: &Path) -> io::Result<Vec<PathBuf>> {
        Ok(vec![
            "/sys/class/input/event5".into(),
            "/sys/class/input/mouse0".into(),
        ])
    }
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        if path == Path::new("/sys/class/input/event5") {
            Ok("/sys/devices/virtual/input/input5/event5".into())
        } else {
            Err(io::Error::from(io::ErrorKind::NotFound))
        }
    }
    fn incarnation(&self, _: &Path) -> io::Result<Incarnation> {
        let mut count = self.inspections.borrow_mut();
        *count += 1;
        Ok(Incarnation {
            device: 1,
            inode: if self.replace && *count > 2 { 2 } else { 1 },
        })
    }
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.reads.borrow_mut().push(path.into());
        if self.denied.is_some_and(|suffix| path.ends_with(suffix)) {
            return Err(io::Error::from_raw_os_error(13));
        }
        if path.ends_with("capabilities/key") {
            return Ok(typing_bitmap().into_bytes());
        }
        let value = if path.ends_with("capabilities/ev") {
            "3\n"
        } else if path.ends_with("uevent") {
            "DEVNAME=input/event5\n"
        } else if path.ends_with("bustype") {
            "0006\n"
        } else if path.ends_with("name") {
            "Synthetic virtual keyboard\n"
        } else {
            "0\n"
        };
        Ok(value.as_bytes().to_vec())
    }
    fn input_properties(&self, _: &Path) -> InputProperties {
        self.roles.clone()
    }
}
fn source() -> Synthetic {
    Synthetic {
        reads: RefCell::new(vec![]),
        denied: None,
        replace: false,
        inspections: RefCell::new(0),
        roles: InputProperties::new(),
    }
}
#[test]
fn actual_metadata_decoding_discovers_a_keyboard_without_opening_an_event_node() {
    let collector = Collector::new(source(), "/sys".into(), Some(64));
    let scan = collector.scan();
    assert!(scan.complete);
    assert!(scan.problems.is_empty());
    assert_eq!(scan.endpoints.len(), 1);
    let endpoint = &scan.endpoints[0];
    assert_eq!(
        endpoint.devnode.known().map(PathBuf::as_path),
        Some(Path::new("/dev/input/event5"))
    );
    assert_eq!(endpoint.classification.kind, InputClass::Keyboard);
    assert_eq!(endpoint.stream_access, StreamAccess::NotChecked);
    assert_eq!(scan.groups[0].usb, Metadata::Missing);
    assert!(
        collector
            .reader
            .reads
            .borrow()
            .iter()
            .all(|path| path.starts_with("/sys"))
    );
}
#[test]
fn denied_capability_metadata_preserves_membership_and_reports_the_exact_path() {
    let mut reader = source();
    reader.denied = Some("capabilities/key");
    let scan = Collector::new(reader, "/sys".into(), Some(64)).scan();
    assert!(scan.complete);
    assert_eq!(scan.endpoints.len(), 1);
    assert_eq!(scan.endpoints[0].classification.kind, InputClass::Unknown);
    assert!(
        scan.problems
            .iter()
            .any(|problem| problem.kind == ProblemKind::Permission
                && problem.path.ends_with("capabilities/key"))
    );
}

#[test]
fn unreadable_bus_metadata_keeps_a_new_endpoint_discoverable_with_unknown_grouping() {
    let mut reader = source();
    reader.denied = Some("id/bustype");
    let scan = Collector::new(reader, "/sys".into(), Some(64)).scan();
    assert!(scan.complete);
    assert_eq!(scan.endpoints.len(), 1);
    assert!(!scan.endpoints[0].group_confirmed);
    assert!(matches!(
        scan.endpoints[0].identity.bus,
        Metadata::Failed(_)
    ));
    assert_eq!(scan.groups[0].usb, Metadata::Missing);
}
#[test]
fn replacement_during_collection_cannot_publish_mixed_metadata() {
    let mut reader = source();
    reader.replace = true;
    let scan = Collector::new(reader, "/sys".into(), Some(64)).scan();
    assert!(!scan.complete);
    assert!(scan.endpoints.is_empty());
    assert_eq!(scan.problems[0].kind, ProblemKind::Race);
}

/// Exercise real symlink/metadata reads while replacing only udev property enrichment.
struct TreeReader;
impl Reader for TreeReader {
    fn entries(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        SystemMetadata.entries(path)
    }
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        SystemMetadata.canonicalize(path)
    }
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        SystemMetadata.read(path)
    }
    fn incarnation(&self, path: &Path) -> io::Result<Incarnation> {
        SystemMetadata.incarnation(path)
    }
    fn input_properties(&self, _: &Path) -> InputProperties {
        InputProperties::new()
    }
}
fn write(path: impl AsRef<Path>, text: &str) {
    let path = path.as_ref();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}
fn add_node(sys: &Path, attachment: &str, number: u32, bus: &str) -> PathBuf {
    let group = sys.join("devices").join(attachment);
    let input = group.join(format!("interface/input/input{number}"));
    let event = input.join(format!("event{number}"));
    fs::create_dir_all(&event).unwrap();
    fs::create_dir_all(sys.join("class/input")).unwrap();
    fs::create_dir_all(sys.join("bus/usb")).unwrap();
    if !group.join("subsystem").exists() {
        std::os::unix::fs::symlink(sys.join("bus/usb"), group.join("subsystem")).unwrap();
    }
    std::os::unix::fs::symlink(&event, sys.join(format!("class/input/event{number}"))).unwrap();
    write(group.join("uevent"), "DEVTYPE=usb_device\n");
    write(group.join("idVendor"), "fffe\n");
    write(group.join("idProduct"), "0001\n");
    write(group.join("serial"), "Identical advertised serial \n");
    for name in ["vendor", "product", "version"] {
        write(input.join("id").join(name), "0000\n");
    }
    write(input.join("id/bustype"), bus);
    write(input.join("capabilities/ev"), "20003\n");
    write(input.join("capabilities/key"), &typing_bitmap());
    for name in ["rel", "abs", "msc", "sw", "led", "snd", "ff"] {
        write(input.join("capabilities").join(name), "0\n");
    }
    write(input.join("properties"), "0\n");
    write(input.join("name"), "Synthetic endpoint\n");
    write(input.join("phys"), "\n");
    write(input.join("uniq"), "\n");
    write(
        event.join("uevent"),
        &format!("DEVNAME=input/event{number}\n"),
    );
    input
}

fn typing_bitmap() -> String {
    format!(
        "{:x}\n",
        crate::input::classification::typing_keys().fold(0u64, |bits, key| bits | (1u64 << key))
    )
}

#[test]
fn virtual_typing_devices_keep_virtual_origin_even_with_an_advertised_usb_bus() {
    let temp = tempfile::tempdir().unwrap();
    let sys = temp.path().join("sys");
    add_node(&sys, "virtual/input", 0, "0003\n");
    let scan = Collector::new(TreeReader, sys, Some(64)).scan();
    assert_eq!(scan.endpoints[0].classification.kind, InputClass::Keyboard);
    assert_eq!(scan.endpoints[0].placement.origin, InputOrigin::Virtual);
    assert_eq!(scan.endpoints[0].placement.location, InputLocation::Unknown);
}

#[test]
fn at_keyboards_need_portable_chassis_evidence_for_a_builtin_location() {
    for chassis in [Some("9\n"), Some("3\n"), None, Some("broken\n")] {
        let temp = tempfile::tempdir().unwrap();
        let sys = temp.path().join("sys");
        let input = add_node(&sys, "platform/i8042/serio0", 0, "0011\n");
        write(input.join("name"), "AT Translated Set 2 keyboard\n");
        if let Some(value) = chassis {
            write(sys.join("class/dmi/id/chassis_type"), value);
        }
        let scan = Collector::new(TreeReader, sys, Some(64)).scan();
        assert_eq!(scan.endpoints[0].classification.kind, InputClass::Keyboard);
        assert_eq!(
            scan.endpoints[0].placement.location,
            if chassis == Some("9\n") {
                InputLocation::Internal
            } else {
                InputLocation::Unknown
            }
        );
        if chassis == Some("broken\n") {
            assert!(
                scan.problems
                    .iter()
                    .any(|p| p.operation.contains("chassis"))
            );
        }
    }
}

#[test]
fn usb_placement_follows_the_entire_chain_and_never_assumes_usb_means_external() {
    for (child, parent, expected) in [
        ("fixed\n", "removable\n", InputLocation::External),
        ("fixed\n", "fixed\n", InputLocation::Internal),
        ("unknown\n", "fixed\n", InputLocation::Unknown),
        ("fixed\n", "unknown\n", InputLocation::Unknown),
        ("removable\n", "unknown\n", InputLocation::External),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let sys = temp.path().join("sys");
        add_node(&sys, "usb1/1-1", 0, "0003\n");
        add_node(&sys, "usb1/1-1/1-1.1", 1, "0003\n");
        let root = sys.join("devices/usb1");
        std::os::unix::fs::symlink(sys.join("bus/usb"), root.join("subsystem")).unwrap();
        write(root.join("uevent"), "DEVTYPE=usb_device\n");
        write(root.join("1-1/removable"), parent);
        write(root.join("1-1/1-1.1/removable"), child);
        let scan = Collector::new(TreeReader, sys, Some(64)).scan();
        let endpoint = scan
            .endpoints
            .iter()
            .find(|e| e.key.0.ends_with("event1"))
            .unwrap();
        assert_eq!(endpoint.placement.location, expected);
        assert_eq!(endpoint.placement.origin, InputOrigin::Physical);
        assert_eq!(endpoint.placement.facts.len(), 2);
    }
}

#[test]
fn role_metadata_preserves_conflicts_and_parse_failures_in_the_inventory() {
    let mut reader = source();
    reader
        .roles
        .insert("ID_INPUT_KEYBOARD", Metadata::Known("0".into()));
    let scan = Collector::new(reader, "/sys".into(), Some(64)).scan();
    assert_eq!(
        scan.endpoints[0].classification.reason,
        ClassificationReason::ConflictingMetadata
    );
    assert_eq!(
        scan.endpoints[0].input_properties["ID_INPUT_KEYBOARD"]
            .known()
            .unwrap(),
        "0"
    );
    let mut reader = source();
    reader.roles.insert(
        "ID_INPUT_MOUSE",
        Metadata::Failed(invalid(
            Path::new("/sys/synthetic"),
            "decode ID_INPUT_MOUSE",
            ProblemKind::Malformed,
        )),
    );
    let scan = Collector::new(reader, "/sys".into(), Some(64)).scan();
    assert!(scan.complete);
    assert_eq!(scan.endpoints[0].classification.kind, InputClass::Unknown);
    assert!(
        scan.problems
            .iter()
            .any(|p| p.kind == ProblemKind::Malformed)
    );
}

struct DeniedPlacement;
impl Reader for DeniedPlacement {
    fn entries(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        TreeReader.entries(path)
    }
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        TreeReader.canonicalize(path)
    }
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        if path.ends_with("chassis_type") || path.ends_with("removable") {
            Err(io::Error::from_raw_os_error(13))
        } else {
            TreeReader.read(path)
        }
    }
    fn incarnation(&self, path: &Path) -> io::Result<Incarnation> {
        TreeReader.incarnation(path)
    }
    fn input_properties(&self, path: &Path) -> InputProperties {
        TreeReader.input_properties(path)
    }
}

#[test]
fn denied_placement_metadata_preserves_keyboard_classification_and_reports_unknown_location() {
    let temp = tempfile::tempdir().unwrap();
    let sys = temp.path().join("sys");
    add_node(&sys, "platform/i8042/serio0", 0, "0011\n");
    add_node(&sys, "usb1/1-1", 1, "0003\n");
    let scan = Collector::new(DeniedPlacement, sys, Some(64)).scan();
    assert!(scan.complete);
    assert_eq!(scan.endpoints.len(), 2);
    for endpoint in &scan.endpoints {
        assert_eq!(endpoint.classification.kind, InputClass::Keyboard);
        assert_eq!(endpoint.placement.location, InputLocation::Unknown);
        assert!(
            endpoint.placement.facts.iter().any(
                |f| matches!(&f.value, Metadata::Failed(p) if p.kind == ProblemKind::Permission)
            )
        );
    }
    assert!(
        scan.problems
            .iter()
            .any(|p| p.path.ends_with("chassis_type"))
    );
    assert!(scan.problems.iter().any(|p| p.path.ends_with("removable")));
}
#[test]
fn usb_siblings_share_an_attachment_but_duplicate_serials_and_bluetooth_do_not_merge_units() {
    let temp = tempfile::tempdir().unwrap();
    let sys = temp.path().join("sys");
    add_node(&sys, "usb1/1-1", 1, "0003\n");
    add_node(&sys, "usb1/1-1", 2, "0003\n");
    add_node(&sys, "usb1/1-2", 3, "0003\n");
    add_node(&sys, "usb1/radio", 4, "0005\n");
    let scan = Collector::new(TreeReader, sys, Some(64)).scan();
    assert!(scan.complete, "{:?}", scan.problems);
    assert!(scan.problems.is_empty(), "{:?}", scan.problems);
    assert_eq!(scan.endpoints.len(), 4);
    assert_eq!(scan.groups.len(), 3);
    assert_eq!(
        scan.groups
            .iter()
            .filter(|group| group.usb.known().is_some())
            .count(),
        2
    );
    assert_eq!(
        scan.groups
            .iter()
            .filter(|group| group
                .serial
                .known()
                .is_some_and(|serial| serial.ends_with(' ')))
            .count(),
        2
    );
    let bt = scan
        .endpoints
        .iter()
        .find(|endpoint| endpoint.identity.bus == Metadata::Known(5))
        .unwrap();
    assert_eq!(
        scan.groups
            .iter()
            .find(|group| group.key == bt.group)
            .unwrap()
            .usb,
        Metadata::Missing
    );
}
#[test]
fn an_inaccessible_root_is_not_an_empty_inventory_and_truncated_fields_are_not_known_empty() {
    let temp = tempfile::tempdir().unwrap();
    let sys = temp.path().join("sys");
    let collector = Collector::new(TreeReader, sys.clone(), Some(64));
    assert!(!collector.scan().complete);
    let input = add_node(&sys, "virtual", 0, "0006\n");
    write(input.join("capabilities/key"), "1");
    let scan = collector.scan();
    assert!(scan.complete);
    assert_eq!(scan.endpoints.len(), 1);
    assert!(matches!(
        scan.endpoints[0].capabilities.codes[&1],
        Metadata::Failed(_)
    ));
    assert_eq!(
        scan.endpoints[0].physical_path,
        Metadata::Known(String::new())
    );
    assert_eq!(scan.endpoints[0].identity.version, Metadata::Known(0));
}
