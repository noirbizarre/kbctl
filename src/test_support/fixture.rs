//! Strict data-only fixtures. Bad protocol bytes are valid; bad schemas are not.
use super::transport::{Action, Controller, Operation, Script, ScriptError, Step};
use crate::{model::connection::TransportKind, transport::TransportFailure};
use miette::Diagnostic;
use serde::Deserialize;
use std::collections::{BTreeSet, VecDeque};
use thiserror::Error;

#[derive(Debug, Error, Diagnostic)]
pub(crate) enum FixtureError {
    #[error("cannot parse protocol fixture `{label}`")]
    #[diagnostic(
        code(kbctl::test_support::fixture_parse),
        help("Use the documented versioned schema; unknown fields and operation tags are errors.")
    )]
    Parse {
        label: String,
        #[source]
        source: serde_json::Error,
    },
    #[error(transparent)]
    #[diagnostic(transparent)]
    Invalid(#[from] ScriptError),
}
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Origin {
    Synthetic {
        procedure: String,
        sanitization: String,
    },
    Captured {
        procedure: String,
        sanitization: String,
        source: String,
    },
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeviceMetadata {
    pub(crate) model: String,
    pub(crate) revision: String,
    pub(crate) firmware: String,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProtocolMetadata {
    pub(crate) id: String,
    pub(crate) version: String,
}
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    Serial,
    Hid,
}
impl Kind {
    fn transport(self) -> TransportKind {
        match self {
            Self::Serial => TransportKind::Serial,
            Self::Hid => TransportKind::Hid,
        }
    }
}
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Failure {
    Disconnected,
    AccessDenied,
    Busy,
    Other,
}
impl Failure {
    fn transport(self) -> TransportFailure {
        match self {
            Self::Disconnected => TransportFailure::Disconnected,
            Self::AccessDenied => TransportFailure::AccessDenied,
            Self::Busy => TransportFailure::Busy,
            Self::Other => TransportFailure::Other,
        }
    }
}
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Outcome {
    Input {
        bytes: Vec<u8>,
        chunk: Option<usize>,
    },
    Output {
        bytes: Vec<u8>,
        #[serde(default)]
        accept: Vec<usize>,
    },
    Failure {
        code: Failure,
        #[serde(default)]
        terminal: bool,
    },
    Eof,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Transfer {
    pub(crate) id: String,
    pub(crate) op: Operation,
    pub(crate) report_id: Option<u8>,
    pub(crate) after: Option<String>,
    pub(crate) releases: Option<String>,
    pub(crate) outcome: Outcome,
}
impl Transfer {
    fn step(&self) -> Step {
        Step {
            label: self.id.clone(),
            op: self.op,
            report: self.report_id,
            after: self.after.clone(),
            releases: self.releases.clone(),
            action: match &self.outcome {
                Outcome::Input { bytes, chunk } => Action::Input {
                    bytes: bytes.clone(),
                    chunk: *chunk,
                },
                Outcome::Output { bytes, accept } => Action::Output {
                    bytes: bytes.clone(),
                    accept: VecDeque::from(accept.clone()),
                },
                Outcome::Failure { code, terminal } => Action::Failure {
                    kind: code.transport(),
                    terminal: *terminal,
                },
                Outcome::Eof => Action::Eof,
            },
        }
    }
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EndpointFixture {
    pub(crate) alias: String,
    pub(crate) transport: Kind,
    pub(crate) gates: Vec<String>,
    pub(crate) idle: Vec<Operation>,
    pub(crate) steps: Vec<Transfer>,
}
impl EndpointFixture {
    fn script(&self) -> Script {
        Script {
            steps: self.steps.iter().map(Transfer::step).collect(),
            gates: self.gates.iter().cloned().collect(),
            idle: self.idle.iter().copied().collect(),
        }
    }
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema_version: u32,
    id: String,
    origin: Origin,
    device: DeviceMetadata,
    protocol: ProtocolMetadata,
    purpose: String,
    unknown_metadata_reason: Option<String>,
    endpoints: Vec<EndpointFixture>,
}
#[derive(Debug, Clone)]
pub(crate) struct Fixture {
    document: Document,
    mutations: Vec<String>,
}
impl Fixture {
    pub(crate) fn parse(label: &str, json: &str) -> Result<Self, FixtureError> {
        let document = serde_json::from_str(json).map_err(|source| FixtureError::Parse {
            label: label.into(),
            source,
        })?;
        let fixture = Self {
            document,
            mutations: vec![],
        };
        fixture.validate()?;
        Ok(fixture)
    }
    fn error(&self, message: impl Into<String>) -> ScriptError {
        ScriptError::new(&self.label(), message)
    }
    pub(crate) fn id(&self) -> &str {
        &self.document.id
    }
    pub(crate) fn label(&self) -> String {
        if self.mutations.is_empty() {
            self.id().into()
        } else {
            format!("{} [{}]", self.id(), self.mutations.join(", "))
        }
    }
    pub(crate) fn origin(&self) -> &Origin {
        &self.document.origin
    }
    pub(crate) fn device(&self) -> &DeviceMetadata {
        &self.document.device
    }
    pub(crate) fn require_protocol(&self, id: &str, version: &str) -> Result<(), ScriptError> {
        if self.document.protocol.id != id || self.document.protocol.version != version {
            return Err(self.error("fixture protocol identity/version differs from this adapter"));
        }
        Ok(())
    }
    pub(crate) fn bind(&self, alias: &str) -> Result<Controller, ScriptError> {
        let endpoint = self
            .document
            .endpoints
            .iter()
            .find(|endpoint| endpoint.alias == alias)
            .ok_or_else(|| self.error("unknown endpoint alias"))?;
        Controller::new(
            &format!("{}:{alias}", self.label()),
            endpoint.transport.transport(),
            endpoint.script(),
        )
    }
    fn validate(&self) -> Result<(), ScriptError> {
        let doc = &self.document;
        if doc.schema_version != 1 {
            return Err(self.error("unsupported fixture schema version"));
        }
        let origin_fields: Vec<&str> = match &doc.origin {
            Origin::Synthetic {
                procedure,
                sanitization,
            } => vec![procedure, sanitization],
            Origin::Captured {
                procedure,
                sanitization,
                source,
            } => vec![procedure, sanitization, source],
        };
        if [
            doc.id.as_str(),
            &doc.purpose,
            &doc.device.model,
            &doc.device.revision,
            &doc.device.firmware,
            &doc.protocol.id,
            &doc.protocol.version,
        ]
        .into_iter()
        .chain(origin_fields)
        .any(|value| value.trim().is_empty())
        {
            return Err(
                self.error("fixture provenance, device and protocol labels must be explicit")
            );
        }
        if [
            &doc.device.revision,
            &doc.device.firmware,
            &doc.protocol.version,
        ]
        .iter()
        .any(|value| value.as_str() == "unknown")
            && doc
                .unknown_metadata_reason
                .as_ref()
                .is_none_or(|reason| reason.trim().is_empty())
        {
            return Err(self.error("unknown version/revision labels need a reason"));
        }
        if doc.endpoints.is_empty() || doc.endpoints.len() > 64 {
            return Err(self.error("fixture needs 1..=64 endpoints"));
        }
        let mut aliases = BTreeSet::new();
        for endpoint in &doc.endpoints {
            if endpoint.alias.trim().is_empty()
                || !aliases.insert(&endpoint.alias)
                || endpoint.steps.len() > 8192
                || endpoint.gates.iter().collect::<BTreeSet<_>>().len() != endpoint.gates.len()
                || endpoint.idle.iter().collect::<BTreeSet<_>>().len() != endpoint.idle.len()
            {
                return Err(self.error("duplicate alias/gate/idle entry or excessive script steps"));
            }
            endpoint
                .script()
                .validate(&self.label(), endpoint.transport.transport())?;
        }
        Ok(())
    }
    fn mutate(
        &self,
        description: String,
        change: impl FnOnce(&mut Document) -> Result<(), ScriptError>,
    ) -> Result<Self, ScriptError> {
        let mut fixture = self.clone();
        change(&mut fixture.document)?;
        fixture.mutations.push(description);
        fixture.validate()?;
        Ok(fixture)
    }
    pub(crate) fn fragment(&self, alias: &str, limit: usize) -> Result<Self, ScriptError> {
        self.mutate(format!("fragment {alias} at {limit} bytes"), |doc| {
            let endpoint = endpoint_mut(doc, alias)?;
            if !matches!(endpoint.transport, Kind::Serial) || limit == 0 {
                return Err(ScriptError::new(
                    alias,
                    "fragmentation needs serial input and nonzero size",
                ));
            }
            for step in &mut endpoint.steps {
                if let Outcome::Input { chunk, .. } = &mut step.outcome {
                    *chunk = Some(limit);
                }
            }
            Ok(())
        })
    }
    pub(crate) fn short_write(
        &self,
        alias: &str,
        step: &str,
        counts: Vec<usize>,
    ) -> Result<Self, ScriptError> {
        self.mutate(format!("short write {alias}/{step} {counts:?}"), |doc| {
            let transfer = transfer_mut(doc, alias, step)?;
            let Outcome::Output { accept, .. } = &mut transfer.outcome else {
                return Err(ScriptError::new(step, "not an output step"));
            };
            *accept = counts;
            Ok(())
        })
    }
    pub(crate) fn truncate(
        &self,
        alias: &str,
        step: &str,
        length: usize,
    ) -> Result<Self, ScriptError> {
        self.mutate(format!("truncate {alias}/{step} to {length}"), |doc| {
            let endpoint = endpoint_mut(doc, alias)?;
            let index = endpoint
                .steps
                .iter()
                .position(|transfer| transfer.id == step)
                .ok_or_else(|| ScriptError::new(step, "unknown step"))?;
            let Outcome::Input { bytes, .. } = &mut endpoint.steps[index].outcome else {
                return Err(ScriptError::new(step, "not an input step"));
            };
            if length > bytes.len() {
                return Err(ScriptError::new(step, "truncation cannot extend input"));
            }
            if length == 0 && endpoint.steps[index].op == Operation::SerialRead {
                endpoint.steps.remove(index);
            } else if let Outcome::Input { bytes, .. } = &mut endpoint.steps[index].outcome {
                bytes.truncate(length);
            }
            Ok(())
        })
    }
    pub(crate) fn replace_byte(
        &self,
        alias: &str,
        step: &str,
        offset: usize,
        value: u8,
    ) -> Result<Self, ScriptError> {
        self.mutate(
            format!("replace {alias}/{step}[{offset}] with {value}"),
            |doc| {
                let transfer = transfer_mut(doc, alias, step)?;
                let bytes = match &mut transfer.outcome {
                    Outcome::Input { bytes, .. } | Outcome::Output { bytes, .. } => bytes,
                    _ => return Err(ScriptError::new(step, "step has no bytes")),
                };
                *bytes
                    .get_mut(offset)
                    .ok_or_else(|| ScriptError::new(step, "byte offset out of range"))? = value;
                Ok(())
            },
        )
    }
    pub(crate) fn coalesce(&self, alias: &str) -> Result<Self, ScriptError> {
        self.mutate(format!("coalesce {alias} ready input"), |doc| {
            let endpoint = endpoint_mut(doc, alias)?;
            if !matches!(endpoint.transport, Kind::Serial) {
                return Err(ScriptError::new(alias, "cannot coalesce HID reports"));
            }
            let mut merged: Vec<Transfer> = vec![];
            for mut step in std::mem::take(&mut endpoint.steps) {
                if step.op == Operation::SerialRead
                    && step.releases.is_none()
                    && let Outcome::Input { bytes, chunk } = &mut step.outcome
                {
                    *chunk = None;
                    if let Some(previous) = merged.last_mut()
                        && previous.op == step.op
                        && previous.after == step.after
                        && previous.releases.is_none()
                        && let Outcome::Input {
                            bytes: prior,
                            chunk,
                        } = &mut previous.outcome
                    {
                        *chunk = None;
                        prior.extend_from_slice(bytes);
                        continue;
                    }
                }
                merged.push(step);
            }
            endpoint.steps = merged;
            Ok(())
        })
    }
    pub(crate) fn end_after(
        &self,
        alias: &str,
        step: &str,
        failure: Option<Failure>,
    ) -> Result<Self, ScriptError> {
        self.mutate(format!("end {alias} after {step}: {failure:?}"), |doc| {
            let endpoint = endpoint_mut(doc, alias)?;
            let index = endpoint
                .steps
                .iter()
                .position(|transfer| transfer.id == step)
                .ok_or_else(|| ScriptError::new(step, "unknown step"))?;
            let op = endpoint.steps[index].op;
            let report_id = endpoint.steps[index].report_id;
            if failure.is_none() && op != Operation::SerialRead {
                return Err(ScriptError::new(step, "EOF requires serial read"));
            }
            let outcome = match failure {
                Some(code) => Outcome::Failure {
                    code,
                    terminal: true,
                },
                None => Outcome::Eof,
            };
            let terminal = Transfer {
                id: format!("{step}-terminal"),
                op,
                report_id,
                after: None,
                releases: None,
                outcome,
            };
            let mut tail = endpoint.steps.split_off(index + 1);
            tail.retain(|transfer| transfer.op != op);
            endpoint.steps.push(terminal);
            endpoint.steps.extend(tail);
            Ok(())
        })
    }
}
fn endpoint_mut<'a>(
    doc: &'a mut Document,
    alias: &str,
) -> Result<&'a mut EndpointFixture, ScriptError> {
    doc.endpoints
        .iter_mut()
        .find(|endpoint| endpoint.alias == alias)
        .ok_or_else(|| ScriptError::new(alias, "unknown endpoint alias"))
}
fn transfer_mut<'a>(
    doc: &'a mut Document,
    alias: &str,
    step: &str,
) -> Result<&'a mut Transfer, ScriptError> {
    endpoint_mut(doc, alias)?
        .steps
        .iter_mut()
        .find(|transfer| transfer.id == step)
        .ok_or_else(|| ScriptError::new(step, "unknown step"))
}

pub(crate) const CORPUS: &[(&str, &str)] = &[
    (
        "battery",
        include_str!("../../tests/fixtures/backends/v1/synthetic/battery.json"),
    ),
    (
        "firmware",
        include_str!("../../tests/fixtures/backends/v1/synthetic/firmware.json"),
    ),
    (
        "event",
        include_str!("../../tests/fixtures/backends/v1/synthetic/event.json"),
    ),
    (
        "malformed",
        include_str!("../../tests/fixtures/backends/v1/synthetic/malformed.json"),
    ),
    (
        "truncated",
        include_str!("../../tests/fixtures/backends/v1/synthetic/truncated.json"),
    ),
];
pub(crate) fn load(name: &str) -> Fixture {
    let (_, json) = CORPUS
        .iter()
        .find(|(id, _)| *id == name)
        .expect("registered fixture");
    Fixture::parse(name, json).unwrap()
}

#[cfg(test)]
mod tests;
