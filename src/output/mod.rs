//! Explicit version-one presentation schema; domain layout is not a serialization contract.
// Unsupported platforms render errors today but still compile and test the shared presentation views.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use crate::{
    application::{Operation, devices::Catalog},
    error::{Error, Result},
    indicators::{AccessError, ErrorKind},
    model::{
        identity::Identification,
        input::*,
        observation::{HostLock, Observation},
        state::*,
    },
};
use miette::Diagnostic;
use serde::Serialize;
use serde_json::{Value, json};
use std::{io::Write, path::Path};

mod human;
pub(crate) use human::{Options as HumanOptions, render as human};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Notice {
    code: String,
    kind: &'static str,
    path: String,
    operation: String,
    os_code: Option<i32>,
    message: String,
    help: String,
}
impl Notice {
    pub(crate) fn discovery(problem: &Problem) -> Self {
        Self {
            code: "kbctl::input::discovery".into(),
            kind: problem_kind(problem.kind),
            path: problem.path.to_string_lossy().into_owned(),
            operation: problem.operation.clone(),
            os_code: problem.os_code,
            message: problem.to_string(),
            help: problem.help().map(|h| h.to_string()).unwrap_or_default(),
        }
    }
    pub(crate) fn indicator(problem: &AccessError) -> Self {
        Self {
            code: "kbctl::indicators::access".into(),
            kind: match problem.kind {
                ErrorKind::Permission => "permission_denied",
                ErrorKind::Inaccessible => "unavailable",
                ErrorKind::Unsupported => "unsupported",
                ErrorKind::Binding => "invalid_binding",
                ErrorKind::Protocol => "error",
            },
            path: problem.path.to_string_lossy().into_owned(),
            operation: problem.operation.into(),
            os_code: problem.source.as_ref().and_then(|e| e.raw_os_error()),
            message: problem.to_string(),
            help: problem.help().map(|h| h.to_string()).unwrap_or_default(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Document {
    #[serde(skip)]
    pub(crate) human_view: Option<human::View>,
    pub(crate) schema_version: u8,
    pub(crate) command: &'static str,
    pub(crate) event: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    cursor: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    time_domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<Value>,
}

impl Document {
    pub(crate) fn failure(operation: &Operation, error: &Error) -> Self {
        Self {
            human_view: None,
            schema_version: 1,
            command: operation.name(),
            event: "error",
            cursor: None,
            time_domain: None,
            data: None,
            error: Some(
                json!({"code": error.code().map(|c| c.to_string()).unwrap_or_else(|| "kbctl::cli::error".into()),
                "message": error.to_string(), "help": error.help().map(|h| h.to_string())}),
            ),
        }
    }

    pub(crate) fn view(
        operation: &Operation,
        event: &'static str,
        snapshot: &Snapshot,
        notices: &[Notice],
    ) -> Result<Self> {
        let mut catalog = Catalog::new(snapshot);
        let selected = match operation {
            Operation::Inspect(selector) => Some(catalog.resolve(selector, true)?),
            Operation::Indicators {
                device: Some(selector),
            } => Some(catalog.resolve(Path::new(selector), false)?),
            _ => None,
        };
        let selected_group = selected.as_ref().map(|s| s.device.group.id);
        let selected_endpoint = selected.as_ref().and_then(|s| s.endpoint);
        catalog
            .devices
            .retain(|d| selected_group.is_none_or(|id| d.group.id == id));
        let notices: Vec<_> = notices
            .iter()
            .filter(|notice| {
                selected_group.is_none()
                    || notice.code == "kbctl::input::discovery"
                    || catalog.devices.iter().any(|d| {
                        d.endpoints.iter().any(|e| {
                            path(&e.endpoint.descriptor.key.0) == notice.path
                                || e.endpoint
                                    .descriptor
                                    .devnode
                                    .known()
                                    .is_some_and(|p| path(p) == notice.path)
                        })
                    })
            })
            .cloned()
            .collect();
        let groups: Vec<_> = snapshot
            .records
            .values()
            .filter_map(|r| match r.as_ref() {
                StateRecord::InputDevice(group)
                    if selected_group.is_none_or(|id| group.id == id) =>
                {
                    Some(group.as_ref())
                }
                _ => None,
            })
            .collect();
        let endpoints: Vec<_> = snapshot
            .records
            .values()
            .filter_map(|r| match r.as_ref() {
                StateRecord::InputEndpoint(endpoint)
                    if selected_group.is_none_or(|id| endpoint.device == id) =>
                {
                    Some(endpoint.as_ref())
                }
                _ => None,
            })
            .collect();
        let discovery_status = snapshot
            .records
            .values()
            .find_map(|r| match r.as_ref() {
                StateRecord::InputDiscovery(status) => Some(status.clone()),
                _ => None,
            })
            .unwrap_or_default();
        let discovery = discovery(&discovery_status);
        let mut devices: Vec<_> = groups.iter().map(|group| {
            let view = catalog.devices.iter().find(|d| d.group.id == group.id).expect("catalog includes each input group");
            let mut members: Vec<_> = endpoints.iter().filter(|e| e.device == group.id).map(|e| endpoint(e)).collect();
            members.sort_by(|a, b| a["syspath"].as_str().cmp(&b["syspath"].as_str()));
            json!({"id": group.id.to_string(), "syspath": path(&group.descriptor.key.0),
                "display_name": view.name, "selector": view.selector, "grouping": view.grouping,
                "usb": metadata(&group.descriptor.usb, |usb| json!({"vendor": usb.vendor, "product": usb.product})),
                "serial": metadata(&group.descriptor.serial, |s| json!(s)),
                "identification": match &group.descriptor.identification {
                    Identification::Unknown => json!({"state": "unknown"}),
                    Identification::Identified(model) => json!({"state": "identified", "model": model}),
                    Identification::Ambiguous(models) => json!({"state": "ambiguous", "models": models}),
                }, "physical_id": group.physical.map(|id| id.to_string()), "endpoints": members})
        }).collect();
        devices.sort_by(|a, b| a["syspath"].as_str().cmp(&b["syspath"].as_str()));
        let mut indicators: Vec<_> = snapshot.records.values().filter_map(|record| {
            let StateRecord::Host(host) = record.as_ref() else { return None };
            let endpoint = endpoints.iter().find(|e| Some(e.id) == host.input);
            if selected_group.is_some() && endpoint.is_none() { return None; }
            Some(json!({"source_id": host.id.to_string(), "generation": host.generation.to_string(),
                "scope": {"namespace": host.scope.namespace(), "value": host.scope.value()},
                "endpoint_id": host.input.map(|id| id.to_string()),
                "syspath": endpoint.map(|e| path(&e.descriptor.key.0)),
                "devnode": endpoint.map(|e| metadata(&e.descriptor.devnode, |p| json!(path(p)))),
                "availability": match host.availability {
                    SourceAvailability::Active => json!({"state": "active"}),
                    SourceAvailability::Unavailable(cause) => json!({"state": "unavailable", "cause": failure(cause)}),
                }, "locks": {"caps_lock": host_field(host, HostLock::Caps),
                     "num_lock": host_field(host, HostLock::Num), "scroll_lock": host_field(host, HostLock::Scroll)}}))
        }).collect();
        indicators.sort_by(|a, b| a["syspath"].as_str().cmp(&b["syspath"].as_str()));
        let grouped: Vec<_> = catalog.devices.iter().filter(|d| d.has_indicators() || selected_group.is_some()).map(|d| json!({
            "device_id": d.group.id.to_string(), "display_name": d.name, "selector": d.selector, "grouping": d.grouping,
            "endpoint_ids": d.endpoints.iter().filter(|e| e.eligible || e.host.is_some()).map(|e| e.endpoint.id.to_string()).collect::<Vec<_>>(),
            "locks": {"caps_lock": d.lock(HostLock::Caps), "num_lock": d.lock(HostLock::Num), "scroll_lock": d.lock(HostLock::Scroll)}
        })).collect();
        let indicator_data = json!({"scope": "per_endpoint_kernel_state", "sources": indicators, "devices": grouped,
            "collection": if indicators.is_empty() { "no_sources" } else { "observed" }});
        let data = match operation {
            Operation::Devices => {
                json!({"discovery": discovery, "devices": devices, "problems": notices})
            }
            Operation::Inspect(_) => {
                let device = devices.into_iter().next().expect("resolved device");
                json!({"discovery": discovery, "device": device, "selected_endpoint_id": selected_endpoint.map(|id| id.to_string()), "problems": notices})
            }
            Operation::Indicators { .. } | Operation::Watch => {
                json!({"discovery": discovery, "indicators": indicator_data, "problems": notices})
            }
            Operation::Status => {
                json!({"execution": "direct", "discovery": discovery, "devices": devices,
                "indicators": indicator_data, "management": {"state": "unavailable", "reason": "no_backend_sessions_started",
                    "keyboard_count": snapshot.records.values().filter(|r| matches!(r.as_ref(), StateRecord::Keyboard { .. })).count(),
                    "session_count": snapshot.records.values().filter(|r| matches!(r.as_ref(), StateRecord::Session { .. })).count()},
                "problems": notices})
            }
        };
        Ok(Self {
            human_view: Some(human::View {
                devices: catalog.devices,
                discovery: discovery_status,
                notices,
                selected_endpoint,
                selected: selected_group.is_some(),
            }),
            schema_version: 1,
            command: operation.name(),
            event,
            cursor: Some(
                json!({"epoch": epoch(snapshot.cursor.epoch), "revision": snapshot.cursor.revision}),
            ),
            time_domain: Some(epoch(snapshot.time_domain)),
            data: Some(data),
            error: None,
        })
    }
}

fn epoch(epoch: Epoch) -> String {
    epoch.bytes().iter().map(|b| format!("{b:02x}")).collect()
}
fn path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
fn metadata<T>(value: &Metadata<T>, render: impl FnOnce(&T) -> Value) -> Value {
    match value {
        Metadata::Known(value) => json!({"state": "known", "value": render(value)}),
        Metadata::Missing => json!({"state": "missing"}),
        Metadata::Failed(problem) => {
            json!({"state": "failed", "problem": Notice::discovery(problem)})
        }
    }
}
fn problem_kind(kind: ProblemKind) -> &'static str {
    match kind {
        ProblemKind::Permission => "permission_denied",
        ProblemKind::Io => "io_error",
        ProblemKind::Malformed => "malformed",
        ProblemKind::Race => "race",
        ProblemKind::Monitor => "monitor_error",
        ProblemKind::Unsupported => "unsupported",
    }
}
fn discovery(status: &DiscoveryStatus) -> Value {
    json!({"inventory": match &status.inventory {
        InventoryStatus::NotScanned => json!({"state": "not_scanned"}), InventoryStatus::Current => json!({"state": "current"}),
        InventoryStatus::Degraded(problems) => json!({"state": "degraded", "problems": problems.iter().map(Notice::discovery).collect::<Vec<_>>()}),
    }, "monitor": match &status.monitor {
        MonitorStatus::NotStarted => json!({"state": "not_started"}), MonitorStatus::Watching => json!({"state": "watching"}),
        MonitorStatus::Degraded(problem) => json!({"state": "degraded", "problem": Notice::discovery(problem)}),
    }})
}
fn endpoint(endpoint: &InputEndpoint) -> Value {
    let d = &endpoint.descriptor;
    json!({"id": endpoint.id.to_string(), "syspath": path(&d.key.0), "group_confirmed": d.group_confirmed,
        "devnode": metadata(&d.devnode, |p| json!(path(p))), "name": metadata(&d.name, |s| json!(s)),
        "physical_path": metadata(&d.physical_path, |s| json!(s)), "unique_name": metadata(&d.unique_name, |s| json!(s)),
        "identity": {"bus": metadata(&d.identity.bus, |v| json!(v)), "vendor": metadata(&d.identity.vendor, |v| json!(v)),
            "product": metadata(&d.identity.product, |v| json!(v)), "version": metadata(&d.identity.version, |v| json!(v))},
        "keyboard": match d.keyboard {
            KeyboardClassification::Unknown => json!({"state": "unknown"}),
            KeyboardClassification::Known { keyboard, evidence } => json!({"state": "known", "value": keyboard,
                "evidence": match evidence { ClassificationEvidence::Udev => "udev", ClassificationEvidence::KeyBitmap => "key_bitmap" }}),
        }, "classification_property": metadata(&d.classification_property, |v| json!(v)), "stream_access": "not_checked",
        "capabilities": {"events": metadata(&d.capabilities.events, |v| json!(v)),
            "codes": d.capabilities.codes.iter().map(|(event, codes)| (event.to_string(), metadata(codes, |v| json!(v)))).collect::<serde_json::Map<_, _>>(),
            "properties": metadata(&d.capabilities.properties, |v| json!(v))}})
}
fn failure(cause: SourceFailure) -> &'static str {
    match cause {
        SourceFailure::PermissionDenied => "permission_denied",
        SourceFailure::Busy => "busy",
        SourceFailure::Suspended => "suspended",
        SourceFailure::Disconnected => "disconnected",
        SourceFailure::Protocol => "protocol_error",
        SourceFailure::Timeout => "timeout",
        SourceFailure::Cancelled => "cancelled",
        SourceFailure::Shutdown => "shutdown",
        SourceFailure::InvalidBinding => "invalid_binding",
        SourceFailure::EventGap => "event_gap",
        SourceFailure::HostUnsupported => "unsupported",
        SourceFailure::HostInaccessible => "inaccessible",
    }
}
fn stamp(stamp: &ReportStamp) -> Value {
    let source = match &stamp.source {
        Provenance::Host {
            source,
            generation,
            scope,
        } => json!({"kind": "host", "source_id": source.to_string(),
            "generation": generation.to_string(), "scope": {"namespace": scope.namespace(), "value": scope.value()}}),
        Provenance::Device(source) => {
            json!({"kind": "device", "reporter": source.reporter, "session_id": source.session.to_string()})
        }
    };
    json!({"source": source, "sequence": stamp.sequence, "received_at_ms": stamp.received_at.0.as_millis(),
        "fresh_until_ms": stamp.fresh_until.0.as_millis()})
}
fn host_field(host: &HostState, lock: HostLock) -> Value {
    let mut value = field(host.fields.get(&lock));
    if let SourceAvailability::Unavailable(cause) = host.availability {
        // An open failure can precede the first report; do not hide it behind an absent field.
        value["source_failure"] = json!(failure(cause));
        value["state"] = json!(match cause {
            SourceFailure::PermissionDenied => "permission_denied",
            SourceFailure::HostUnsupported => "unsupported",
            SourceFailure::Protocol | SourceFailure::InvalidBinding => "error",
            _ => "unavailable",
        });
    } else {
        value["source_failure"] = Value::Null;
    }
    value
}

fn field(field: Option<&FieldState<bool>>) -> Value {
    let Some(field) = field else {
        return json!({"state": "unknown", "quality": "unknown", "value": null,
        "report": null, "failure": null, "last_known": null});
    };
    let current = field.report.as_ref().and_then(|r| match r.value {
        Observation::Known(value) => Some(value),
        Observation::Unknown | Observation::Unavailable(_) => None,
    });
    let quality = match field.quality() {
        FieldQuality::Unknown => "unknown",
        FieldQuality::Fresh => "fresh",
        FieldQuality::Stale => "stale",
        FieldQuality::Failed => "failed",
    };
    let state = if let Some(fact) = field.failure {
        match fact.cause {
            SourceFailure::PermissionDenied => "permission_denied",
            SourceFailure::HostUnsupported => "unsupported",
            SourceFailure::HostInaccessible
            | SourceFailure::Disconnected
            | SourceFailure::EventGap
            | SourceFailure::Shutdown => "unavailable",
            _ => "error",
        }
    } else if quality == "stale" {
        "stale"
    } else if current.is_some() {
        "known"
    } else {
        "unknown"
    };
    json!({"state": state, "quality": quality, "value": current,
        "report": field.report.as_ref().map(|r| json!({"stamp": stamp(&r.stamp), "validity": match r.validity {
            Validity::Fresh => json!({"state": "fresh"}), Validity::Stale { since, cause } => json!({"state": "stale", "since_ms": since.0.as_millis(),
                "cause": match cause { StaleCause::Expired => "expired", StaleCause::Replaced => "replaced", StaleCause::Source(cause) => failure(cause) }}),
        }})), "failure": field.failure.map(|f| json!({"cause": failure(f.cause), "at_ms": f.at.0.as_millis()})),
        "last_known": field.last_known.as_ref().map(|s| json!({"value": s.value, "stamp": stamp(&s.stamp)}))})
}

pub(crate) fn stdout(document: &Document, json: bool, detail: bool) -> Result<()> {
    if json {
        // The library owns destination/color policy, including FORCE_COLOR and NO_COLOR handling.
        console_json::write_json_stdout(document)?;
        let mut out = std::io::stdout().lock();
        writeln!(out)
            .and_then(|()| out.flush())
            .map_err(|e| Error::io("finish JSON output", e))
    } else {
        human(
            &mut std::io::stdout().lock(),
            document,
            HumanOptions::stdout(detail),
        )
    }
}
