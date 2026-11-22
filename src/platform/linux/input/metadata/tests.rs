use super::*;
use std::cell::RefCell;

struct Synthetic {
    reads: RefCell<Vec<PathBuf>>,
    denied: Option<&'static str>,
    replace: bool,
    inspections: RefCell<usize>,
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
        let value = if path.ends_with("capabilities/ev") {
            "3\n"
        } else if path.ends_with("capabilities/key") {
            "3ff0000\n"
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
    fn keyboard_property(&self, _: &Path) -> Metadata<String> {
        Metadata::Missing
    }
}
fn source() -> Synthetic {
    Synthetic {
        reads: RefCell::new(vec![]),
        denied: None,
        replace: false,
        inspections: RefCell::new(0),
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
    assert!(matches!(
        endpoint.keyboard,
        KeyboardClassification::Known { keyboard: true, .. }
    ));
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
    assert_eq!(scan.endpoints[0].keyboard, KeyboardClassification::Unknown);
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
    fn keyboard_property(&self, _: &Path) -> Metadata<String> {
        Metadata::Missing
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
    write(input.join("capabilities/key"), "3ff0000\n");
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
