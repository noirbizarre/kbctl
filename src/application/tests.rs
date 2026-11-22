use super::*;
use crate::{
    error::Error,
    indicators::{AccessError, Drain, ErrorKind, Fallback, Lease, Mode},
    model::{
        input::*,
        state::{HostUnavailable, HostValue, StateRecord},
    },
    output,
    test_support::{
        clock::ManualClock,
        input::{failure, scan},
    },
};
use serde_json::Value;
use std::{cell::RefCell, collections::VecDeque, io, rc::Rc, time::Duration};

struct FakeSource {
    scan: Rc<RefCell<Scan>>,
    monitor_failure: bool,
}
impl Source for FakeSource {
    fn start_monitor(&mut self) -> std::result::Result<(), Problem> {
        if self.monitor_failure {
            Err(failure())
        } else {
            Ok(())
        }
    }
    fn drain(&mut self, _: usize) -> std::result::Result<input::Drain, Problem> {
        Ok(input::Drain::default())
    }
    fn scan(&mut self) -> Scan {
        self.scan.borrow().clone()
    }
}
#[derive(Default)]
struct Access {
    opens: usize,
    live: usize,
    denied: bool,
    unknown: bool,
    #[cfg(target_os = "linux")]
    owner: Option<Arc<crate::platform::linux::ownership::OwnerGuard>>,
}
struct FakeOpener(Rc<RefCell<Access>>);
struct FakeLease {
    access: Rc<RefCell<Access>>,
    value: bool,
}
impl Drop for FakeLease {
    fn drop(&mut self) {
        self.access.borrow_mut().live -= 1;
    }
}
impl Opener for FakeOpener {
    type Lease = FakeLease;
    fn open(&mut self, endpoint: &InputEndpoint) -> std::result::Result<FakeLease, AccessError> {
        let mut access = self.0.borrow_mut();
        access.opens += 1;
        if access.denied {
            return Err(AccessError {
                path: endpoint.descriptor.devnode.known().unwrap().clone(),
                operation: "open indicators",
                kind: ErrorKind::Permission,
                source: Some(io::Error::from(io::ErrorKind::PermissionDenied)),
            });
        }
        access.live += 1;
        Ok(FakeLease {
            access: self.0.clone(),
            value: access.opens % 2 == 1,
        })
    }
}
impl Lease for FakeLease {
    fn mode(&self) -> Mode {
        Mode::Snapshots(Fallback::Kernel)
    }
    fn snapshot(&mut self) -> std::result::Result<indicators::Snapshot, AccessError> {
        let value = if self.access.borrow().unknown {
            HostValue::Unknown
        } else {
            HostValue::Known(self.value)
        };
        self.value = !self.value;
        Ok(indicators::Snapshot {
            caps: value,
            num: HostValue::Known(false),
            scroll: HostValue::Unavailable(HostUnavailable::Unsupported),
        })
    }
    fn drain(&mut self, _: usize) -> std::result::Result<Drain, AccessError> {
        panic!("snapshot mode must not read the event queue")
    }
    fn downgrade(&mut self, _: Fallback) {}
}
enum Turn {
    Step,
    Expire,
    Overflow,
    Remove,
    Cancel,
}
struct FakeHost {
    app: Application<FakeSource, FakeOpener>,
    clock: Arc<ManualClock>,
    access: Rc<RefCell<Access>>,
    scan: Rc<RefCell<Scan>>,
    turns: VecDeque<Turn>,
    stopped: bool,
}
impl FakeHost {
    fn new(scan: Scan, indicators: bool) -> Self {
        let clock = Arc::new(ManualClock::new());
        let scan = Rc::new(RefCell::new(scan));
        let access = Rc::new(RefCell::new(Access::default()));
        let app = Application::new(
            FakeSource {
                scan: scan.clone(),
                monitor_failure: false,
            },
            indicators.then(|| FakeOpener(access.clone())),
            crate::registry::builtin::registry().unwrap(),
            clock.clone(),
        )
        .unwrap();
        Self {
            app,
            clock,
            access,
            scan,
            turns: VecDeque::new(),
            stopped: false,
        }
    }
    fn run(&mut self, operation: &Operation) -> Vec<Document> {
        let mut output = vec![];
        execute(self, operation, |document| {
            output.push(document.clone());
            Ok(())
        })
        .unwrap();
        assert!(self.stopped);
        assert_eq!(self.access.borrow().live, 0);
        output
    }
}
impl Host for FakeHost {
    fn initialize(&mut self) -> Result<()> {
        self.app.initialize()
    }
    fn reader(&self) -> StateReader {
        self.app.runtime.reader()
    }
    fn notices(&self) -> &[Notice] {
        &self.app.notices
    }
    fn stop(&mut self) -> Result<()> {
        self.stopped = true;
        self.app.stop()
    }
    fn turn(&mut self) -> Result<bool> {
        self.clock.advance(Duration::from_secs(1));
        match self.turns.pop_front().unwrap_or(Turn::Cancel) {
            Turn::Cancel => return Ok(false),
            Turn::Step => self.app.step(&[])?,
            Turn::Expire => {
                self.clock.advance(Duration::from_secs(3));
                self.app.runtime.advance_time(self.clock.now())?;
            }
            Turn::Overflow => {
                for _ in 0..70 {
                    self.clock.advance(Duration::from_secs(1));
                    self.app.step(&[])?;
                }
            }
            Turn::Remove => {
                self.scan.borrow_mut().groups.clear();
                self.scan.borrow_mut().endpoints.clear();
                self.clock.advance(Duration::from_secs(30));
                self.app.step(&[])?;
            }
        }
        Ok(true)
    }
}
fn data(document: &Document) -> &Value {
    document.data.as_ref().unwrap()
}
fn locks(document: &Document) -> &Value {
    &data(document)["indicators"]["sources"][0]["locks"]
}
fn many(count: usize) -> Scan {
    let mut result = scan();
    let template = result.endpoints[0].clone();
    result.endpoints = (0..count)
        .map(|index| {
            let mut endpoint = template.clone();
            endpoint.key = DiscoveryKey(
                format!("/sys/devices/synthetic-usb/input{index}/event{index}").into(),
            );
            endpoint.devnode = Metadata::Known(format!("/dev/input/event{index}").into());
            endpoint.incarnation.as_mut().unwrap().inode = index as u64 + 2;
            endpoint
        })
        .collect();
    result
}

#[test]
fn device_commands_include_group_context_and_never_open_indicator_nodes() {
    for selector in [
        None,
        Some("/sys/devices/synthetic-usb"),
        Some("/dev/input/event0"),
        Some("/sys/devices/synthetic-usb/input0/event0"),
    ] {
        let operation = selector.map_or(Operation::Devices, |s| Operation::Inspect(s.into()));
        let mut host = FakeHost::new(scan(), false);
        let documents = host.run(&operation);
        let value = data(&documents[0]);
        let group = if selector.is_some() {
            &value["device"]
        } else {
            &value["devices"][0]
        };
        assert_eq!(group["syspath"], "/sys/devices/synthetic-usb");
        assert_eq!(group["endpoints"][0]["stream_access"], "not_checked");
        assert_eq!(host.access.borrow().opens, 0);
    }
}

#[test]
fn absent_and_ambiguous_selectors_are_actionable_failures() {
    for ambiguous in [false, true] {
        let mut fixture = many(if ambiguous { 2 } else { 1 });
        if ambiguous {
            fixture.endpoints[1].devnode = fixture.endpoints[0].devnode.clone();
        }
        let mut host = FakeHost::new(fixture, false);
        let operation = Operation::Inspect(
            if ambiguous {
                "/dev/input/event0"
            } else {
                "/dev/input/absent"
            }
            .into(),
        );
        let error = execute(&mut host, &operation, |_| Ok(())).unwrap_err();
        assert!(
            matches!(error, Error::Selection { matches, .. } if matches == if ambiguous { 2 } else { 0 })
        );
        let document = serde_json::to_value(Document::failure(&operation, &error)).unwrap();
        assert_eq!(document["error"]["code"], "kbctl::cli::device_selection");
        assert!(
            document["error"]["help"]
                .as_str()
                .unwrap()
                .contains("device list")
        );
        assert!(host.stopped);
    }
}

#[test]
fn empty_discovery_is_distinct_from_a_permission_failed_inventory() {
    for denied in [false, true] {
        let mut fixture = many(0);
        fixture.groups.clear();
        fixture.complete = !denied;
        if denied {
            fixture.problems.push(failure());
        }
        let mut host = FakeHost::new(fixture, true);
        let documents = host.run(&Operation::Status);
        let value = data(&documents[0]);
        assert_eq!(value["devices"], serde_json::json!([]));
        assert_eq!(
            value["discovery"]["inventory"]["state"],
            if denied { "degraded" } else { "current" }
        );
        if denied {
            assert_eq!(value["problems"][0]["kind"], "permission_denied");
        }
        assert_eq!(value["indicators"]["collection"], "no_sources");
    }
}

#[test]
fn unknown_metadata_and_non_keyboard_groups_remain_visible() {
    let mut fixture = many(2);
    fixture.endpoints[0].keyboard = KeyboardClassification::Unknown;
    fixture.endpoints[0].name = Metadata::Failed(failure());
    fixture.endpoints[1].keyboard = KeyboardClassification::Known {
        keyboard: false,
        evidence: ClassificationEvidence::Udev,
    };
    let documents = FakeHost::new(fixture, false).run(&Operation::Devices);
    let endpoints = &data(&documents[0])["devices"][0]["endpoints"];
    assert_eq!(endpoints[0]["keyboard"]["state"], "unknown");
    assert_eq!(endpoints[0]["name"]["problem"]["kind"], "permission_denied");
    assert_eq!(endpoints[1]["keyboard"]["value"], false);
}

#[test]
fn indicators_distinguish_true_false_unknown_unsupported_and_permission_denied() {
    for (denied, unknown) in [(false, false), (false, true), (true, false)] {
        let mut host = FakeHost::new(scan(), true);
        host.access.borrow_mut().denied = denied;
        host.access.borrow_mut().unknown = unknown;
        let documents = host.run(&Operation::Indicators);
        let locks = locks(&documents[0]);
        assert_eq!(
            locks["caps_lock"]["state"],
            if denied {
                "permission_denied"
            } else if unknown {
                "unknown"
            } else {
                "known"
            }
        );
        if denied {
            assert!(locks["caps_lock"]["value"].is_null());
            assert_eq!(
                data(&documents[0])["problems"][0]["kind"],
                "permission_denied"
            );
        } else {
            assert_eq!(locks["num_lock"]["value"], false);
            assert_eq!(locks["scroll_lock"]["state"], "unsupported");
            assert!(locks["scroll_lock"]["value"].is_null());
        }
    }
}

#[test]
fn initial_collection_covers_more_than_one_budget_and_preserves_endpoint_disagreement() {
    let mut host = FakeHost::new(many(65), true);
    let documents = host.run(&Operation::Status);
    let sources = data(&documents[0])["indicators"]["sources"]
        .as_array()
        .unwrap();
    assert_eq!(host.access.borrow().opens, 65);
    assert_eq!(sources.len(), 65);
    assert!(
        sources
            .iter()
            .any(|s| s["locks"]["caps_lock"]["value"] == true)
    );
    assert!(
        sources
            .iter()
            .any(|s| s["locks"]["caps_lock"]["value"] == false)
    );
}

#[test]
fn watch_emits_initial_changes_expiry_resync_and_removal_then_closes_leases() {
    let mut host = FakeHost::new(scan(), true);
    host.turns = [
        Turn::Step,
        Turn::Expire,
        Turn::Overflow,
        Turn::Remove,
        Turn::Cancel,
    ]
    .into();
    let documents = host.run(&Operation::Watch);
    assert_eq!(documents[0].event, "snapshot");
    assert_ne!(
        locks(&documents[0])["caps_lock"]["value"],
        locks(&documents[1])["caps_lock"]["value"]
    );
    assert_eq!(locks(&documents[2])["caps_lock"]["state"], "stale");
    assert!(documents.iter().any(|d| d.event == "resync"));
    assert_eq!(
        data(documents.last().unwrap())["indicators"]["sources"],
        serde_json::json!([])
    );
}

#[test]
fn idle_watch_cancellation_and_output_failure_both_retire_sources() {
    for broken in [false, true] {
        let mut host = FakeHost::new(scan(), true);
        let result = execute(&mut host, &Operation::Watch, |_| {
            if broken {
                Err(Error::io(
                    "write fixture output",
                    io::Error::from(io::ErrorKind::BrokenPipe),
                ))
            } else {
                Ok(())
            }
        });
        if broken {
            assert!(result.unwrap_err().is_broken_pipe());
        } else {
            result.unwrap();
        }
        assert!(host.stopped);
        assert_eq!(host.access.borrow().live, 0);
        assert!(
            !host
                .reader()
                .snapshot()
                .unwrap()
                .records
                .values()
                .any(|r| matches!(r.as_ref(), StateRecord::Host(_)))
        );
    }
}

#[test]
fn human_and_json_render_the_same_failure_and_indicator_facts() {
    let mut host = FakeHost::new(scan(), true);
    let documents = host.run(&Operation::Status);
    let document = &documents[0];
    let json = serde_json::to_value(document).unwrap();
    let mut text = vec![];
    output::human(&mut text, document).unwrap();
    let text = String::from_utf8(text).unwrap();
    fn check(value: &Value, text: &str) {
        match value {
            Value::Object(fields) => {
                for (key, value) in fields {
                    assert!(text.contains(&key.replace('_', " ")));
                    check(value, text);
                }
            }
            Value::Array(items) => {
                for item in items {
                    check(item, text);
                }
            }
            Value::String(value) => assert!(
                text.contains(
                    &value
                        .chars()
                        .flat_map(|c| c.escape_debug())
                        .collect::<String>()
                )
            ),
            Value::Bool(value) => assert!(text.contains(&value.to_string())),
            Value::Number(value) => assert!(text.contains(&value.to_string())),
            Value::Null => assert!(text.contains("(not present)")),
        }
    }
    check(&json, &text);
}

#[cfg(target_os = "linux")]
#[test]
fn ownership_contention_never_admits_a_second_opener_and_cleanup_releases_the_guard() {
    use crate::platform::linux::ownership::{OwnerGuard, OwnershipError};
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut host = FakeHost::new(scan(), true);
    host.access.borrow_mut().owner =
        Some(Arc::new(OwnerGuard::for_test(directory.path()).unwrap()));
    host.initialize().unwrap();
    assert!(matches!(
        OwnerGuard::for_test(directory.path()),
        Err(OwnershipError::Busy(_))
    ));
    assert_eq!(host.access.borrow().opens, 1);
    host.stop().unwrap();
    assert_eq!(host.access.borrow().live, 0);
    host.access.borrow_mut().owner = None;
    assert!(OwnerGuard::for_test(directory.path()).is_ok());
}
