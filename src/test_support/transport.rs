//! Protocol-independent scripted I/O with separate lanes and explicit dependencies.
use crate::{
    model::{
        connection::{ConnectionState, TransportKind},
        identity::EndpointId,
        topology::Topology,
    },
    transport::{
        AccessGrant, IoRequest, LeaseSet, OwnershipReservation, TransportError, TransportFailure,
        TransportIo, TransportLease,
    },
};
use miette::Diagnostic;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    io,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
};
use thiserror::Error;

#[derive(Debug, Clone, Error, Diagnostic, PartialEq, Eq)]
#[error("test script `{label}`: {detail}")]
#[diagnostic(
    code(kbctl::test_support::script),
    help(
        "Check the fixture operation, gate and byte transcript; do not hide unexpected I/O with an idle tail."
    )
)]
pub(crate) struct ScriptError {
    pub(crate) label: String,
    pub(crate) detail: String,
}
impl ScriptError {
    pub(crate) fn new(label: &str, detail: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            detail: detail.into(),
        }
    }
}
pub(crate) type Result<T> = std::result::Result<T, ScriptError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Operation {
    SerialRead,
    SerialWrite,
    HidInput,
    HidOutput,
    HidGetFeature,
    HidSetFeature,
}
impl Operation {
    pub(crate) fn input(self) -> bool {
        matches!(
            self,
            Self::SerialRead | Self::HidInput | Self::HidGetFeature
        )
    }
    pub(crate) fn kind(self) -> TransportKind {
        if matches!(self, Self::SerialRead | Self::SerialWrite) {
            TransportKind::Serial
        } else {
            TransportKind::Hid
        }
    }
    pub(crate) fn report_required(self) -> bool {
        matches!(
            self,
            Self::HidOutput | Self::HidGetFeature | Self::HidSetFeature
        )
    }
}

#[derive(Debug, Clone)]
pub(crate) enum Action {
    Input {
        bytes: Vec<u8>,
        chunk: Option<usize>,
    },
    Output {
        bytes: Vec<u8>,
        accept: VecDeque<usize>,
    },
    Failure {
        kind: TransportFailure,
        terminal: bool,
    },
    Eof,
}
#[derive(Debug, Clone)]
pub(crate) struct Step {
    pub(crate) label: String,
    pub(crate) op: Operation,
    pub(crate) report: Option<u8>,
    pub(crate) after: Option<String>,
    pub(crate) releases: Option<String>,
    pub(crate) action: Action,
}
impl Step {
    pub(crate) fn input(bytes: Vec<u8>) -> Self {
        Self {
            label: "input".into(),
            op: Operation::SerialRead,
            report: None,
            after: None,
            releases: None,
            action: Action::Input { bytes, chunk: None },
        }
    }
    pub(crate) fn output(bytes: Vec<u8>) -> Self {
        Self {
            label: "output".into(),
            op: Operation::SerialWrite,
            report: None,
            after: None,
            releases: None,
            action: Action::Output {
                bytes,
                accept: VecDeque::new(),
            },
        }
    }
    pub(crate) fn named(mut self, label: &str) -> Self {
        self.label = label.into();
        self
    }
}
#[derive(Debug, Clone, Default)]
pub(crate) struct Script {
    pub(crate) steps: Vec<Step>,
    pub(crate) gates: BTreeSet<String>,
    pub(crate) idle: BTreeSet<Operation>,
}
impl Script {
    pub(crate) fn validate(&self, label: &str, kind: TransportKind) -> Result<()> {
        if self.gates.iter().any(|gate| gate.trim().is_empty())
            || self.idle.iter().any(|op| op.kind() != kind)
        {
            return Err(ScriptError::new(label, "invalid gate or idle operation"));
        }
        let mut names = BTreeSet::new();
        let mut eof = false;
        for step in &self.steps {
            if step.op == Operation::SerialRead && eof {
                return Err(ScriptError::new(
                    label,
                    "serial input after EOF is unreachable",
                ));
            }
            if matches!(step.action, Action::Eof) {
                eof = true;
            }
            if step.label.trim().is_empty()
                || !names.insert(&step.label)
                || step.op.kind() != kind
                || step.op.report_required() != step.report.is_some()
                || step
                    .after
                    .iter()
                    .chain(&step.releases)
                    .any(|gate| !self.gates.contains(gate))
            {
                return Err(ScriptError::new(
                    label,
                    format!(
                        "invalid step identity, transport, report or gate: {}",
                        step.label
                    ),
                ));
            }
            match &step.action {
                Action::Input { bytes, chunk }
                    if !step.op.input()
                        || bytes.len() > 65536
                        || chunk == &Some(0)
                        || (bytes.is_empty() && step.op == Operation::SerialRead)
                        || (kind == TransportKind::Hid && chunk.is_some()) =>
                {
                    return Err(ScriptError::new(
                        label,
                        "invalid input size/chunk or operation",
                    ));
                }
                Action::Output { bytes, accept } => {
                    if step.op.input()
                        || bytes.len() > 65536
                        || (bytes.is_empty() && step.op == Operation::SerialWrite)
                    {
                        return Err(ScriptError::new(label, "invalid output bytes or operation"));
                    }
                    let mut remaining = bytes.len();
                    for size in accept {
                        if *size > remaining || (kind == TransportKind::Hid && *size != bytes.len())
                        {
                            return Err(ScriptError::new(
                                label,
                                "acceptance counts violate remaining bytes or atomic HID report",
                            ));
                        }
                        remaining -= size;
                    }
                }
                Action::Eof if step.op != Operation::SerialRead => {
                    return Err(ScriptError::new(label, "EOF is a serial-read outcome"));
                }
                _ => {}
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TraceOutcome {
    Pending,
    Progress(usize),
    Failure(String),
    Gate(String),
    Closed,
    Dropped,
    Eof,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Trace {
    pub(crate) op: Option<Operation>,
    pub(crate) report: Option<u8>,
    pub(crate) offered: Vec<u8>,
    pub(crate) transferred: Vec<u8>,
    pub(crate) outcome: TraceOutcome,
}
struct Active {
    step: Step,
    offset: usize,
}
type Reaction = Arc<dyn Fn(&[u8]) -> Vec<Vec<u8>> + Send + Sync>;
struct State {
    label: String,
    kind: TransportKind,
    lanes: BTreeMap<Operation, VecDeque<Active>>,
    gates: BTreeSet<String>,
    released: BTreeSet<String>,
    idle: BTreeSet<Operation>,
    waiters: BTreeMap<Operation, Waker>,
    pending_logged: BTreeSet<Operation>,
    failure: Option<ScriptError>,
    terminal: Option<TransportFailure>,
    eof: bool,
    trace: VecDeque<Trace>,
    trace_dropped: usize,
    read_limit: usize,
    write_limit: usize,
    choices: Vec<Vec<u8>>,
    reaction: Option<Reaction>,
    completed_writes: Vec<Vec<u8>>,
    next_label: usize,
}
impl State {
    fn log(&mut self, trace: Trace) {
        if self.trace.len() == 256 {
            self.trace.pop_front();
            self.trace_dropped += 1;
        }
        self.trace.push_back(trace);
    }
    fn mismatch(&mut self, detail: impl Into<String>) -> ScriptError {
        let failure = ScriptError::new(
            &self.label,
            format!(
                "{}; recent transcript: {:?} ({} earlier entries omitted)",
                detail.into(),
                self.trace,
                self.trace_dropped
            ),
        );
        self.log(Trace {
            op: None,
            report: None,
            offered: vec![],
            transferred: vec![],
            outcome: TraceOutcome::Failure(failure.detail.clone()),
        });
        self.failure = Some(failure.clone());
        failure
    }
    fn release(&mut self, gate: &str) -> Vec<Waker> {
        if self.released.insert(gate.into()) {
            self.log(Trace {
                op: None,
                report: None,
                offered: vec![],
                transferred: vec![],
                outcome: TraceOutcome::Gate(gate.into()),
            });
            self.pending_logged.clear();
            let affected: Vec<_> = self
                .waiters
                .keys()
                .copied()
                .filter(|op| {
                    self.lanes
                        .get(op)
                        .and_then(|lane| lane.front())
                        .is_some_and(|active| active.step.after.as_deref() == Some(gate))
                })
                .collect();
            affected
                .into_iter()
                .filter_map(|op| self.waiters.remove(&op))
                .collect()
        } else {
            vec![]
        }
    }
}
#[derive(Clone)]
pub(crate) struct Controller(Arc<Mutex<State>>);
fn wake(wakers: Vec<Waker>) {
    for waker in wakers {
        waker.wake();
    }
}
impl Controller {
    pub(crate) fn new(label: &str, kind: TransportKind, script: Script) -> Result<Self> {
        script.validate(label, kind)?;
        let mut lanes: BTreeMap<_, VecDeque<_>> = BTreeMap::new();
        for step in script.steps {
            lanes
                .entry(step.op)
                .or_default()
                .push_back(Active { step, offset: 0 });
        }
        Ok(Self(Arc::new(Mutex::new(State {
            label: label.into(),
            kind,
            lanes,
            gates: script.gates,
            released: BTreeSet::new(),
            idle: script.idle,
            waiters: BTreeMap::new(),
            pending_logged: BTreeSet::new(),
            failure: None,
            terminal: None,
            eof: false,
            trace: VecDeque::new(),
            trace_dropped: 0,
            read_limit: usize::MAX,
            write_limit: usize::MAX,
            choices: vec![],
            reaction: None,
            completed_writes: vec![],
            next_label: 0,
        }))))
    }
    pub(crate) fn kind(&self) -> TransportKind {
        self.0.lock().unwrap().kind
    }
    pub(crate) fn transcript(&self) -> Vec<Trace> {
        self.0.lock().unwrap().trace.iter().cloned().collect()
    }
    pub(crate) fn completed_writes(&self) -> Vec<Vec<u8>> {
        self.0.lock().unwrap().completed_writes.clone()
    }
    pub(crate) fn set_read_limit(&self, limit: usize) -> Result<()> {
        if self.kind() == TransportKind::Hid {
            return Err(ScriptError::new(
                "controller",
                "HID reports cannot be stream-fragmented",
            ));
        }
        if limit == 0 {
            return Err(ScriptError::new("controller", "read limit must be nonzero"));
        }
        self.0.lock().unwrap().read_limit = limit;
        Ok(())
    }
    pub(crate) fn set_write_limit(&self, limit: usize) {
        self.0.lock().unwrap().write_limit = limit;
    }
    pub(crate) fn release(&self, gate: &str) -> Result<()> {
        let wakes = {
            let mut state = self.0.lock().unwrap();
            if !state.gates.contains(gate) {
                return Err(ScriptError::new(
                    &state.label,
                    "release references unknown gate",
                ));
            }
            state.release(gate)
        };
        wake(wakes);
        Ok(())
    }
    pub(crate) fn append(&self, mut step: Step) -> Result<()> {
        let wakes = {
            let mut state = self.0.lock().unwrap();
            if state.eof && step.op == Operation::SerialRead {
                return Err(ScriptError::new(
                    &state.label,
                    "cannot append input after EOF",
                ));
            }
            step.label = format!("{}-{}", step.label, state.next_label);
            state.next_label += 1;
            Script {
                steps: vec![step.clone()],
                gates: state.gates.clone(),
                idle: BTreeSet::new(),
            }
            .validate(&state.label, state.kind)?;
            let op = step.op;
            state
                .lanes
                .entry(op)
                .or_default()
                .push_back(Active { step, offset: 0 });
            state.pending_logged.remove(&op);
            state.waiters.remove(&op).into_iter().collect()
        };
        wake(wakes);
        Ok(())
    }
    pub(crate) fn input(&self, bytes: Vec<u8>) {
        self.append(Step::input(bytes)).unwrap();
    }
    pub(crate) fn clear_input(&self) {
        self.0.lock().unwrap().lanes.remove(&Operation::SerialRead);
    }
    pub(crate) fn fault(&self, op: Operation, kind: TransportFailure, terminal: bool) {
        self.append(Step {
            label: "fault".into(),
            op,
            report: None,
            after: None,
            releases: None,
            action: Action::Failure { kind, terminal },
        })
        .unwrap();
    }
    pub(crate) fn disconnect(&self) {
        let wakes = {
            let mut state = self.0.lock().unwrap();
            state.terminal = Some(TransportFailure::Disconnected);
            state.log(Trace {
                op: None,
                report: None,
                offered: vec![],
                transferred: vec![],
                outcome: TraceOutcome::Failure("terminal Disconnected".into()),
            });
            std::mem::take(&mut state.waiters).into_values().collect()
        };
        wake(wakes);
    }
    /// Rust-only toy recipes may react to completed known commands. The generic
    /// transport contains neither opcodes nor protocol parsing, and JSON cannot install callbacks.
    pub(crate) fn reactive_writes(&self, choices: Vec<Vec<u8>>, reaction: Reaction) -> Result<()> {
        if choices.is_empty() || choices.iter().any(Vec::is_empty) {
            return Err(ScriptError::new(
                "reactive output",
                "provide nonempty exact command choices",
            ));
        }
        let old = {
            let mut state = self.0.lock().unwrap();
            state.choices = choices;
            state.reaction.replace(reaction)
        };
        drop(old);
        Ok(())
    }
    pub(crate) fn assert_complete(&self) -> Result<()> {
        let state = self.0.lock().unwrap();
        if let Some(failure) = &state.failure {
            return Err(failure.clone());
        }
        let remaining: Vec<_> = state
            .lanes
            .values()
            .flatten()
            .map(|active| (&active.step.label, active.offset))
            .collect();
        if !remaining.is_empty() {
            return Err(ScriptError::new(
                &state.label,
                format!(
                    "unconsumed steps {remaining:?}; transcript {:?}",
                    state.trace
                ),
            ));
        }
        Ok(())
    }
    pub(crate) fn assert_cancelled_at(&self, label: &str) -> Result<()> {
        let state = self.0.lock().unwrap();
        if let Some(failure) = &state.failure {
            return Err(failure.clone());
        }
        if state
            .lanes
            .values()
            .flatten()
            .filter(|active| active.step.label == label)
            .count()
            != 1
            || !state
                .trace
                .iter()
                .any(|trace| trace.outcome == TraceOutcome::Closed)
        {
            return Err(ScriptError::new(
                &state.label,
                "cancellation must close I/O at one explicit remaining checkpoint",
            ));
        }
        Ok(())
    }
    fn adapter(&self, endpoint: EndpointId, lifecycle: Option<(LifecycleLog, u64)>) -> ScriptedIo {
        ScriptedIo {
            endpoint,
            control: self.clone(),
            lifecycle,
            closed: false,
        }
    }
}

pub(crate) fn pair(
    endpoint: EndpointId,
    kind: TransportKind,
    script: Script,
) -> Result<(impl TransportIo, Controller)> {
    let control = Controller::new("standalone", kind, script)?;
    Ok((control.adapter(endpoint, None), control))
}
struct ScriptedIo {
    endpoint: EndpointId,
    control: Controller,
    lifecycle: Option<(LifecycleLog, u64)>,
    closed: bool,
}
fn io_error(
    endpoint: EndpointId,
    kind: TransportFailure,
    message: impl Into<String>,
) -> TransportError {
    TransportError::Io {
        endpoint,
        kind,
        source: io::Error::other(message.into()),
    }
}
fn script_error(endpoint: EndpointId, error: ScriptError) -> TransportError {
    TransportError::Io {
        endpoint,
        kind: TransportFailure::Other,
        source: io::Error::other(error),
    }
}

fn operation(request: &IoRequest<'_>) -> (Operation, Option<u8>, Vec<u8>, usize) {
    match request {
        IoRequest::SerialRead(data) => (Operation::SerialRead, None, vec![], data.len()),
        IoRequest::SerialWrite(data) => (Operation::SerialWrite, None, data.to_vec(), data.len()),
        IoRequest::HidInput(data) => (Operation::HidInput, None, vec![], data.len()),
        IoRequest::HidOutput { report, data } => (
            Operation::HidOutput,
            Some(*report),
            data.to_vec(),
            data.len(),
        ),
        IoRequest::HidGetFeature { report, data } => {
            (Operation::HidGetFeature, Some(*report), vec![], data.len())
        }
        IoRequest::HidSetFeature { report, data } => (
            Operation::HidSetFeature,
            Some(*report),
            data.to_vec(),
            data.len(),
        ),
    }
}
impl TransportIo for ScriptedIo {
    fn kind(&self) -> TransportKind {
        self.control.kind()
    }
    fn poll_io(
        &mut self,
        cx: &mut Context<'_>,
        mut request: IoRequest<'_>,
    ) -> Poll<crate::transport::Result<usize>> {
        let (op, report, offered, capacity) = operation(&request);
        let mut candidate_waker = Some(cx.waker().clone());
        let old_waker;
        let mut wakes = Vec::new();
        let mut completion = None;
        let (result, reaction) = {
            let mut state = self.control.0.lock().unwrap();
            let result = if self.closed {
                Poll::Ready(Err(io_error(
                    self.endpoint,
                    TransportFailure::Disconnected,
                    "scripted handle is closed",
                )))
            } else if let Some(error) = state.failure.clone() {
                Poll::Ready(Err(script_error(self.endpoint, error)))
            } else if let Some(kind) = state.terminal {
                Poll::Ready(Err(io_error(
                    self.endpoint,
                    kind,
                    "terminal scripted disconnect",
                )))
            } else if op.kind() != state.kind {
                Poll::Ready(Err(TransportError::WrongKind(self.endpoint)))
            } else if (capacity == 0
                && matches!(op, Operation::SerialRead | Operation::SerialWrite))
                || (state.eof && op == Operation::SerialRead)
            {
                Poll::Ready(Ok(0))
            } else {
                let mut lane = state.lanes.remove(&op).unwrap_or_default();
                if lane.is_empty() && op == Operation::SerialWrite && !state.choices.is_empty() {
                    let choices: Vec<_> = state
                        .choices
                        .iter()
                        .filter(|choice| choice.starts_with(&offered))
                        .cloned()
                        .collect();
                    if choices.len() == 1 {
                        lane.push_back(Active {
                            step: Step::output(choices[0].clone()),
                            offset: 0,
                        });
                    }
                }
                let result = process(
                    &mut state,
                    &mut lane,
                    self.endpoint,
                    op,
                    report,
                    &offered,
                    capacity,
                    &mut request,
                    &mut wakes,
                    &mut completion,
                );
                state.lanes.insert(op, lane);
                result
            };
            if result.is_pending() {
                old_waker = state
                    .waiters
                    .insert(op, candidate_waker.take().expect("candidate waker"));
                if state.pending_logged.insert(op) {
                    state.log(Trace {
                        op: Some(op),
                        report,
                        offered: offered.clone(),
                        transferred: vec![],
                        outcome: TraceOutcome::Pending,
                    });
                }
            } else {
                state.pending_logged.remove(&op);
                old_waker = state.waiters.remove(&op);
            }
            (result, state.reaction.clone())
        };
        drop(old_waker);
        drop(candidate_waker);
        // Completion hooks can enqueue input or release gates through the same
        // controller, so callbacks must never run while its mutex is held.
        if let (Some(bytes), Some(reaction)) = (completion, reaction) {
            for input in reaction(&bytes) {
                self.control.input(input);
            }
        }
        wake(wakes);
        result
    }
    fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        let waiters = {
            let mut state = self.control.0.lock().unwrap();
            state.log(Trace {
                op: None,
                report: None,
                offered: vec![],
                transferred: vec![],
                outcome: TraceOutcome::Closed,
            });
            std::mem::take(&mut state.waiters)
        };
        drop(waiters);
        if let Some((log, id)) = &self.lifecycle {
            log.lock().unwrap().push(Lifecycle::Closed {
                reservation: *id,
                endpoint: self.endpoint,
            });
        }
    }
}
impl Drop for ScriptedIo {
    fn drop(&mut self) {
        self.close();
        self.control.0.lock().unwrap().log(Trace {
            op: None,
            report: None,
            offered: vec![],
            transferred: vec![],
            outcome: TraceOutcome::Dropped,
        });
        if let Some((log, id)) = &self.lifecycle {
            log.lock().unwrap().push(Lifecycle::Dropped {
                reservation: *id,
                endpoint: self.endpoint,
            });
        }
    }
}

#[allow(clippy::too_many_arguments)] // One operation transaction keeps all script mutation under its single lock.
fn process(
    state: &mut State,
    lane: &mut VecDeque<Active>,
    endpoint: EndpointId,
    op: Operation,
    report: Option<u8>,
    offered: &[u8],
    capacity: usize,
    request: &mut IoRequest<'_>,
    wakes: &mut Vec<Waker>,
    completion: &mut Option<Vec<u8>>,
) -> Poll<crate::transport::Result<usize>> {
    let mut transferred = Vec::new();
    loop {
        let Some(active) = lane.front_mut() else {
            if !transferred.is_empty() {
                break;
            }
            if state.idle.contains(&op) {
                return Poll::Pending;
            }
            let error = state.mismatch(format!(
                "unexpected {op:?}: script exhausted; offered {offered:?}"
            ));
            return Poll::Ready(Err(script_error(endpoint, error)));
        };
        if active
            .step
            .after
            .as_ref()
            .is_some_and(|gate| !state.released.contains(gate))
        {
            if transferred.is_empty() {
                return Poll::Pending;
            }
            break;
        }
        if active.step.report != report {
            let error = state.mismatch(format!(
                "{} expected report {:?}, got {report:?}",
                active.step.label, active.step.report
            ));
            return Poll::Ready(Err(script_error(endpoint, error)));
        }
        let done;
        match &mut active.step.action {
            Action::Failure { kind, terminal } => {
                if !transferred.is_empty() {
                    break;
                }
                let kind = *kind;
                if *terminal {
                    state.terminal = Some(kind);
                }
                lane.pop_front();
                state.log(Trace {
                    op: Some(op),
                    report,
                    offered: offered.into(),
                    transferred: vec![],
                    outcome: TraceOutcome::Failure(format!("{kind:?}")),
                });
                return Poll::Ready(Err(io_error(
                    endpoint,
                    kind,
                    "synthetic device vanished or denied access",
                )));
            }
            Action::Eof => {
                if !transferred.is_empty() {
                    break;
                }
                lane.pop_front();
                state.eof = true;
                state.log(Trace {
                    op: Some(op),
                    report,
                    offered: vec![],
                    transferred: vec![],
                    outcome: TraceOutcome::Eof,
                });
                return Poll::Ready(Ok(0));
            }
            Action::Input { bytes, chunk } => {
                let remaining = &bytes[active.offset..];
                if op != Operation::SerialRead && remaining.len() > capacity {
                    let error = state.mismatch("HID report does not fit the requested buffer");
                    return Poll::Ready(Err(script_error(endpoint, error)));
                }
                let size = remaining
                    .len()
                    .min(capacity - transferred.len())
                    .min(state.read_limit.saturating_sub(transferred.len()))
                    .min(chunk.unwrap_or(usize::MAX));
                transferred.extend_from_slice(&remaining[..size]);
                active.offset += size;
                done = active.offset == bytes.len();
            }
            Action::Output { bytes, accept } => {
                let remaining = &bytes[active.offset..];
                if (op == Operation::SerialWrite && !remaining.starts_with(offered))
                    || (op != Operation::SerialWrite && remaining != offered)
                {
                    let error = state.mismatch(format!(
                        "{} expected output suffix {remaining:?}, got {offered:?}",
                        active.step.label
                    ));
                    return Poll::Ready(Err(script_error(endpoint, error)));
                }
                let size = accept
                    .pop_front()
                    .unwrap_or(offered.len())
                    .min(state.write_limit);
                if size > offered.len() || (op != Operation::SerialWrite && size != remaining.len())
                {
                    let error = state.mismatch("invalid acceptance count or partial HID output");
                    return Poll::Ready(Err(script_error(endpoint, error)));
                }
                transferred.extend_from_slice(&remaining[..size]);
                active.offset += size;
                done = active.offset == bytes.len();
                if done && op == Operation::SerialWrite {
                    state.completed_writes.push(bytes.clone());
                    *completion = Some(bytes.clone());
                }
            }
        }
        let chunk_boundary = matches!(&active.step.action, Action::Input { chunk: Some(_), .. });
        if done {
            let step = lane.pop_front().unwrap().step;
            if let Some(gate) = step.releases {
                wakes.extend(state.release(&gate));
            }
        }
        if !done
            || op != Operation::SerialRead
            || chunk_boundary
            || transferred.len() == capacity
            || transferred.len() >= state.read_limit
        {
            break;
        }
    }
    match request {
        IoRequest::SerialRead(buffer)
        | IoRequest::HidInput(buffer)
        | IoRequest::HidGetFeature { data: buffer, .. } => {
            buffer[..transferred.len()].copy_from_slice(&transferred)
        }
        _ => {}
    }
    let size = transferred.len();
    state.log(Trace {
        op: Some(op),
        report,
        offered: offered.into(),
        transferred,
        outcome: TraceOutcome::Progress(size),
    });
    Poll::Ready(Ok(size))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Lifecycle {
    Opened {
        reservation: u64,
        endpoint: EndpointId,
    },
    Closed {
        reservation: u64,
        endpoint: EndpointId,
    },
    Dropped {
        reservation: u64,
        endpoint: EndpointId,
    },
    Released {
        reservation: u64,
        endpoints: Vec<EndpointId>,
    },
}
type LifecycleLog = Arc<Mutex<Vec<Lifecycle>>>;
#[derive(Default)]
struct Reservations {
    endpoints: BTreeSet<EndpointId>,
    next: u64,
}
#[derive(Clone, Default)]
pub(crate) struct LeaseFactory {
    reserved: Arc<Mutex<Reservations>>,
    log: LifecycleLog,
}
struct Reservation {
    id: u64,
    endpoints: Vec<EndpointId>,
    factory: LeaseFactory,
}
impl OwnershipReservation for Reservation {}
impl Drop for Reservation {
    fn drop(&mut self) {
        {
            let mut reserved = self.factory.reserved.lock().unwrap();
            for endpoint in &self.endpoints {
                reserved.endpoints.remove(endpoint);
            }
        }
        self.factory.log.lock().unwrap().push(Lifecycle::Released {
            reservation: self.id,
            endpoints: self.endpoints.clone(),
        });
    }
}
impl LeaseFactory {
    pub(crate) fn lifecycle(&self) -> Vec<Lifecycle> {
        self.log.lock().unwrap().clone()
    }
    pub(crate) fn open(
        &self,
        topology: &Topology,
        selected: &[EndpointId],
        channels: &BTreeMap<EndpointId, Controller>,
        grant: AccessGrant,
        bound: usize,
    ) -> crate::transport::Result<LeaseSet> {
        if selected.is_empty()
            || selected.iter().collect::<BTreeSet<_>>().len() != selected.len()
            || bound == 0
        {
            return Err(TransportError::InvalidLease(
                "select distinct channels and a nonzero transfer bound",
            ));
        }
        let mut generations = Vec::new();
        for endpoint in selected {
            let channel = topology
                .endpoint(*endpoint)
                .map_err(|_| TransportError::InvalidLease("unknown selected endpoint"))?;
            let ConnectionState::Ready(generation) = channel.state() else {
                return Err(TransportError::InvalidLease(
                    "revalidate before opening a lease",
                ));
            };
            if !channels
                .get(endpoint)
                .is_some_and(|control| control.kind() == channel.transport())
            {
                return Err(TransportError::InvalidLease(
                    "missing or wrong-kind fake channel",
                ));
            }
            generations.push(generation);
        }
        let id = {
            let mut reserved = self.reserved.lock().unwrap();
            if let Some(endpoint) = selected
                .iter()
                .find(|endpoint| reserved.endpoints.contains(endpoint))
            {
                return Err(io_error(
                    *endpoint,
                    TransportFailure::Busy,
                    "fake opener's cooperative reservation is held",
                ));
            }
            let id = reserved.next;
            reserved.next = id.checked_add(1).ok_or(TransportError::InvalidLease(
                "reservation identities exhausted",
            ))?;
            reserved.endpoints.extend(selected.iter().copied());
            id
        };
        let reservation = Reservation {
            id,
            endpoints: selected.to_vec(),
            factory: self.clone(),
        };
        let mut leases = Vec::new();
        for (endpoint, generation) in selected.iter().zip(generations) {
            self.log.lock().unwrap().push(Lifecycle::Opened {
                reservation: id,
                endpoint: *endpoint,
            });
            leases.push(TransportLease::new(
                *endpoint,
                generation,
                grant,
                Box::new(channels[endpoint].adapter(*endpoint, Some((self.log.clone(), id)))),
                bound,
            )?);
        }
        LeaseSet::new(leases, Box::new(reservation))
    }
    pub(crate) fn assert_released(&self) {
        assert!(
            self.reserved.lock().unwrap().endpoints.is_empty(),
            "fake leases still held"
        );
        let log = self.lifecycle();
        for (release, event) in log.iter().enumerate() {
            if let Lifecycle::Released {
                reservation,
                endpoints,
            } = event
            {
                for endpoint in endpoints {
                    let close = log
                        .iter()
                        .position(|event| {
                            event
                                == &Lifecycle::Closed {
                                    reservation: *reservation,
                                    endpoint: *endpoint,
                                }
                        })
                        .expect("opened endpoint closed");
                    let drop = log
                        .iter()
                        .position(|event| {
                            event
                                == &Lifecycle::Dropped {
                                    reservation: *reservation,
                                    endpoint: *endpoint,
                                }
                        })
                        .expect("I/O owner dropped");
                    assert!(
                        close < drop && drop < release,
                        "reservation released before all I/O owners"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
