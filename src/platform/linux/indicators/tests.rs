use super::*;
use std::collections::VecDeque;
struct Kernel {
    log: Vec<&'static str>,
    setup_failure: Option<usize>,
    setup_calls: usize,
    clock: i32,
    prefilter_keys: usize,
    filtered: bool,
    events: VecDeque<Event>,
    support: u64,
    capability_failure: bool,
}
impl Kernel {
    fn new() -> Self {
        Self {
            log: vec![],
            setup_failure: None,
            setup_calls: 0,
            clock: nix::libc::CLOCK_REALTIME,
            prefilter_keys: 3,
            filtered: false,
            events: VecDeque::new(),
            support: 7,
            capability_failure: false,
        }
    }
    fn setup(&mut self) -> Result<(), AccessError> {
        self.setup_calls += 1;
        if self.setup_failure == Some(self.setup_calls) {
            Err(invalid(
                Path::new("synthetic"),
                "configure filtered notifications",
                ErrorKind::Unsupported,
            ))
        } else {
            Ok(())
        }
    }
}
impl Evdev for Kernel {
    fn support(&mut self) -> Result<u64, AccessError> {
        self.log.push("support");
        if self.capability_failure {
            return Err(invalid(
                Path::new("synthetic"),
                "query indicator capabilities",
                ErrorKind::Inaccessible,
            ));
        }
        Ok(self.support)
    }
    fn values(&mut self) -> Result<u64, AccessError> {
        self.log.push("snapshot");
        Ok(2)
    }
    fn mask(&mut self, kind: u32, bits: u64) -> Result<(), AccessError> {
        self.log
            .push(if kind == 0 { "type mask" } else { "code mask" });
        self.setup()?;
        if kind == 0 {
            assert_eq!(bits, 1 | EV_LED);
            self.filtered = true;
        } else {
            assert_eq!(bits, LOCKS);
        }
        Ok(())
    }
    fn clock(&mut self, clock: i32) -> Result<(), AccessError> {
        self.log.push(if clock == nix::libc::CLOCK_REALTIME {
            "realtime"
        } else {
            "monotonic"
        });
        self.setup()?;
        if self.clock != clock {
            self.prefilter_keys = 0;
            self.events.clear();
            self.events.push_back(Event::Dropped);
            self.clock = clock;
        }
        Ok(())
    }
    fn events(&mut self, budget: usize) -> Result<Vec<Event>, AccessError> {
        self.log.push("read");
        assert!(self.filtered);
        assert_eq!(
            self.prefilter_keys, 0,
            "unfiltered backlog must never enter application memory"
        );
        Ok((0..budget)
            .filter_map(|_| self.events.pop_front())
            .collect())
    }
}
#[test]
fn filters_and_clock_purge_complete_before_the_initial_snapshot_and_any_read() {
    let mut driver = Driver::initialize(Kernel::new(), true).unwrap();
    assert_eq!(
        driver.io.log,
        [
            "support",
            "type mask",
            "code mask",
            "realtime",
            "monotonic",
            "snapshot"
        ]
    );
    assert_eq!(driver.mode(), Mode::Events);
    assert!(driver.snapshot().is_ok());
    driver.io.events.push_back(Event::Report);
    let drain = driver.drain(64).unwrap();
    assert!(drain.gap && drain.synchronized);
}
#[test]
fn every_partial_stream_setup_failure_permits_snapshots_but_no_event_read() {
    for fail in 1..=4 {
        let mut kernel = Kernel::new();
        kernel.setup_failure = Some(fail);
        let mut driver = Driver::initialize(kernel, true).unwrap();
        assert_eq!(driver.mode(), Mode::Snapshots(Fallback::Setup));
        driver.drain(64).unwrap();
        assert!(!driver.io.log.contains(&"read"));
        assert!(driver.snapshot().is_ok());
    }
    let mut old = Driver::initialize(Kernel::new(), false).unwrap();
    old.drain(64).unwrap();
    assert_eq!(old.io.log, ["support", "snapshot"]);
}
#[test]
fn dropped_packets_recover_at_a_report_boundary_and_never_replay_the_rest_of_the_batch() {
    let mut driver = Driver::initialize(Kernel::new(), true).unwrap();
    driver.io.events = VecDeque::from([Event::Dropped, Event::Led]);
    let first = driver.drain(2).unwrap();
    assert!(first.gap);
    assert!(!first.synchronized);
    assert!(!first.changed);
    driver.io.events = VecDeque::from([Event::Report, Event::Dropped]);
    let next = driver.drain(8).unwrap();
    assert!(next.synchronized);
    assert!(!driver.recovering);
}
#[test]
fn unsupported_bits_are_not_false_and_supported_clear_bits_are_false() {
    let snapshot = snapshot(2, 0);
    assert_eq!(snapshot.caps, HostValue::Known(false));
    assert_eq!(
        snapshot.num,
        HostValue::Unavailable(HostUnavailable::Unsupported)
    );
    assert!(!compatible_release("4.3.99"));
    assert!(compatible_release("4.4.0"));
    assert!(compatible_release("6.12.0-custom"));
    assert!(!compatible_release("unknown"));
}

#[test]
fn an_empty_supported_set_is_explicitly_unsupported_but_capability_failure_is_not_empty_support() {
    let mut kernel = Kernel::new();
    kernel.support = 0;
    let mut driver = Driver::initialize(kernel, true).unwrap();
    assert_eq!(driver.mode(), Mode::Unsupported);
    let snapshot = driver.snapshot().unwrap();
    assert_eq!(
        snapshot.caps,
        HostValue::Unavailable(HostUnavailable::Unsupported)
    );
    assert_eq!(snapshot.num, snapshot.caps);
    assert_eq!(snapshot.scroll, snapshot.caps);
    assert_eq!(driver.io.log, ["support"]);
    let mut failing = Kernel::new();
    failing.capability_failure = true;
    assert!(Driver::initialize(failing, true).is_err());
}

fn native_fixture() -> (tempfile::TempDir, SystemIo) {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let owner = Arc::new(OwnerGuard::for_test(directory.path()).unwrap());
    let file = tempfile::tempfile().unwrap();
    let io = SystemIo {
        fd: file.into(),
        _owner: owner,
        path: "synthetic native descriptor".into(),
    };
    (directory, io)
}
#[test]
fn short_native_bitmap_results_are_rejected_even_when_the_buffer_contains_nonzero_data() {
    unsafe fn short(_: i32, bits: &mut [nix::libc::c_ulong]) -> nix::Result<i32> {
        bits[0] = !0;
        Ok(1)
    }
    unsafe fn empty(_: i32, _: &mut [nix::libc::c_ulong]) -> nix::Result<i32> {
        Ok(0)
    }
    unsafe fn complete(_: i32, bits: &mut [nix::libc::c_ulong]) -> nix::Result<i32> {
        bits[0] = 2;
        Ok(std::mem::size_of_val(bits) as i32)
    }
    let (_directory, io) = native_fixture();
    assert_eq!(
        io.bitmap("snapshot", short).unwrap_err().kind,
        ErrorKind::Protocol
    );
    assert_eq!(
        io.bitmap("snapshot", empty).unwrap_err().kind,
        ErrorKind::Protocol
    );
    assert_eq!(io.bitmap("snapshot", complete).unwrap(), 2);
}
#[test]
fn a_native_descriptor_keeps_process_ownership_until_it_has_closed() {
    let (directory, io) = native_fixture();
    assert!(matches!(
        OwnerGuard::for_test(directory.path()),
        Err(super::super::ownership::OwnershipError::Busy(_))
    ));
    drop(io);
    assert!(OwnerGuard::for_test(directory.path()).is_ok());
}
#[test]
fn native_event_reads_reject_partial_records_without_logging_payloads() {
    use std::io::{Seek, Write};
    let (_directory, mut io) = native_fixture();
    let mut file = fs::File::from(io.fd);
    file.write_all(b"x").unwrap();
    file.rewind().unwrap();
    io.fd = file.into();
    let error = io.events(64).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Protocol);
    assert!(!error.to_string().contains("payload"));
}

#[test]
fn stale_bindings_and_regular_file_substitutions_fail_before_indicator_ioctls() {
    use crate::model::identity::{InputDeviceId, InputEndpointId};
    let (directory, io) = native_fixture();
    let mut opener = LinuxOpener::new(io._owner.clone());
    let path = directory.path().join("substituted-node");
    fs::write(&path, b"not an evdev device").unwrap();
    let metadata = fs::metadata(directory.path()).unwrap();
    let mut descriptor = crate::test_support::input::scan().endpoints.remove(0);
    descriptor.key.0 = directory.path().into();
    descriptor.incarnation = Some(Incarnation {
        device: metadata.dev(),
        inode: metadata.ino() + 1,
    });
    descriptor.devnode = crate::model::input::Metadata::Known(path);
    let mut endpoint = InputEndpoint {
        id: InputEndpointId::new().unwrap(),
        device: InputDeviceId::new().unwrap(),
        descriptor,
    };
    let first = opener
        .open(&endpoint)
        .err()
        .expect("stale binding rejected");
    assert_eq!(first.kind, ErrorKind::Binding);
    assert_eq!(first.operation, "reject replaced sysfs attachment");
    endpoint.descriptor.incarnation.as_mut().unwrap().inode = metadata.ino();
    let second = opener.open(&endpoint).err().expect("regular node rejected");
    assert_eq!(second.kind, ErrorKind::Binding);
    assert_eq!(second.operation, "validate character device");
}
