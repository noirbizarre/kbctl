//! Command-specific terminal presentation; selectors and state words remain useful without styling.
use super::{Document, Notice};
use crate::{
    application::devices::{DeviceView, EndpointView, LockObservation, LockState},
    error::{Error, Result},
    model::{
        identity::InputEndpointId,
        input::*,
        observation::HostLock,
        state::{SourceAvailability, Validity},
    },
};
use console::{Style, Term, measure_text_width, truncate_str};
use std::{
    collections::BTreeSet,
    io::{self, Write},
};

#[derive(Clone, Debug)]
pub(crate) struct View {
    pub(crate) devices: Vec<DeviceView>,
    pub(crate) discovery: DiscoveryStatus,
    pub(crate) notices: Vec<Notice>,
    pub(crate) selected_endpoint: Option<InputEndpointId>,
    pub(crate) selected: bool,
    pub(crate) total_inputs: usize,
    pub(crate) all_inputs: bool,
    pub(crate) keyboard_count: usize,
}
#[derive(Clone, Copy)]
pub(crate) struct Options {
    pub(crate) detail: bool,
    pub(crate) width: usize,
    pub(crate) color: bool,
}
impl Options {
    pub(crate) fn stdout(detail: bool) -> Self {
        let term = Term::stdout();
        Self {
            detail,
            width: usize::from(term.size().1),
            color: term.is_term() && console::colors_enabled(),
        }
    }
}
pub(crate) fn render(
    writer: &mut impl Write,
    document: &Document,
    mut options: Options,
) -> Result<()> {
    // Inspection is the explicit diagnostic command, even without a verbosity flag.
    options.detail |= document.command == "device.inspect";
    let mut renderer = Renderer { writer, options };
    renderer
        .document(document)
        .and_then(|()| renderer.writer.flush())
        .map_err(|e| Error::io("write human output", e))
}
struct Renderer<'a, W: Write> {
    writer: &'a mut W,
    options: Options,
}
impl<W: Write> Renderer<'_, W> {
    fn styled(&self, text: &str, style: Style) -> String {
        style
            .force_styling(self.options.color)
            .apply_to(text)
            .to_string()
    }
    fn heading(&mut self, text: &str) -> io::Result<()> {
        writeln!(
            self.writer,
            "{}",
            self.styled(&safe(text), Style::new().bold().cyan())
        )
    }
    fn label(&mut self, label: &str, text: &str) -> io::Result<()> {
        // Wrap labels/metadata using visible widths, never byte offsets into Unicode device names.
        let label = format!("  {label}: ");
        let text = safe(text);
        let available = self
            .options
            .width
            .saturating_sub(measure_text_width(&label))
            .max(12);
        let chunks = wrap(&text, available);
        for (index, chunk) in chunks.iter().enumerate() {
            if index == 0 {
                writeln!(
                    self.writer,
                    "{}{chunk}",
                    self.styled(&label, Style::new().dim())
                )?;
            } else {
                writeln!(self.writer, "    {chunk}")?;
            }
        }
        Ok(())
    }
    fn document(&mut self, document: &Document) -> io::Result<()> {
        let Some(view) = &document.human_view else {
            return Ok(());
        };
        if document.command == "indicator.watch" {
            self.heading(&format!("Indicators ({})", document.event))?;
        }
        self.health(view)?;
        match document.command {
            "device.list" => self.list(view)?,
            "device.inspect" => {
                for device in &view.devices {
                    self.inspect(device, view.selected_endpoint)?;
                }
            }
            "indicator.status" | "indicator.watch" => self.indicators(view)?,
            "status" => {
                self.heading("Keyboard status")?;
                self.label(
                    "Discovery",
                    &format!(
                        "{} recognized keyboard(s), {} input attachment(s); {}",
                        view.keyboard_count,
                        view.total_inputs,
                        if matches!(view.discovery.inventory, InventoryStatus::Current) {
                            "current inventory"
                        } else {
                            "inventory uncertain"
                        }
                    ),
                )?;
                self.label("Management", "unavailable (no backend sessions started)")?;
                if self.options.detail {
                    self.list(view)?;
                }
                self.indicators(view)?;
            }
            _ => {}
        }
        if !view.notices.is_empty() {
            if self.options.detail || document.command == "device.inspect" {
                self.heading("Diagnostics")?;
                for notice in &view.notices {
                    self.label(
                        &notice.kind.replace('_', " "),
                        &format!(
                            "{}: {} at {} (OS code {:?})",
                            notice.code, notice.operation, notice.path, notice.os_code
                        ),
                    )?;
                    self.label("Recovery", &notice.help)?;
                }
            } else {
                self.label(
                    "Diagnostics",
                    &format!(
                        "{} problem(s); use --verbose for details",
                        view.notices.len()
                    ),
                )?;
            }
        }
        Ok(())
    }
    fn health(&mut self, view: &View) -> io::Result<()> {
        let inventory = match &view.discovery.inventory {
            InventoryStatus::Current => None,
            InventoryStatus::NotScanned => Some("not scanned; device inventory is unknown"),
            InventoryStatus::Degraded(_) => {
                Some("degraded; listed attachments may be incomplete or retained")
            }
        };
        if let Some(status) = inventory {
            self.label("Discovery", status)?;
        }
        if let MonitorStatus::Degraded(problem) = &view.discovery.monitor {
            self.label(
                "Monitoring",
                &format!("degraded ({})", super::problem_kind(problem.kind)),
            )?;
        }
        if self.options.detail {
            if let InventoryStatus::Degraded(problems) = &view.discovery.inventory {
                for problem in problems {
                    self.problem(problem)?;
                }
            }
            if let MonitorStatus::Degraded(problem) = &view.discovery.monitor {
                self.problem(problem)?;
            }
        }
        Ok(())
    }
    fn list(&mut self, view: &View) -> io::Result<()> {
        if view.devices.is_empty() {
            return writeln!(
                self.writer,
                "{}",
                if matches!(view.discovery.inventory, InventoryStatus::Current) {
                    if view.all_inputs {
                        "No input devices found."
                    } else {
                        "No keyboards found. Use `device list --all` to inspect all input devices."
                    }
                } else {
                    if view.all_inputs {
                        "No input devices available; discovery is uncertain."
                    } else {
                        "No recognized keyboards available; discovery is uncertain. Use `device list --all` for other inputs."
                    }
                }
            );
        }
        let rows: Vec<_> = view
            .devices
            .iter()
            .map(|d| {
                let mut members: Vec<_> = d.endpoints.iter().collect();
                // The everyday locator should lead to the typing endpoint, not an earlier media-only sibling.
                members.sort_by_key(|e| match e.endpoint.descriptor.classification.kind {
                    InputClass::Keyboard => 0,
                    InputClass::Partial => 1,
                    _ => 2,
                });
                let paths: Vec<_> = members
                    .into_iter()
                    .filter_map(|e| e.endpoint.descriptor.devnode.known())
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect();
                let path = paths
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "unavailable".into());
                let mut path = if d.endpoints.len() > 1 {
                    format!("{path} (+{})", d.endpoints.len() - 1)
                } else {
                    path
                };
                if d.origin == InputOrigin::Virtual {
                    path.push_str(" [virtual]");
                } else if d.location != InputLocation::Unknown {
                    path.push_str(&format!(" [{}]", d.location.name()));
                }
                (safe(&d.name), d.user_selector.id.as_str(), safe(&path))
            })
            .collect();
        let natural_name_width = rows
            .iter()
            .map(|r| measure_text_width(&r.0))
            .max()
            .unwrap_or(4)
            .clamp(4, 24);
        let id_width = rows
            .iter()
            .map(|r| measure_text_width(r.1))
            .max()
            .unwrap_or(8)
            .max(8);
        let path_width = rows
            .iter()
            .map(|r| measure_text_width(&r.2))
            .max()
            .unwrap_or(4);
        let name_width = natural_name_width.min(
            self.options
                .width
                .saturating_sub(id_width + path_width + 4)
                .max(12),
        );
        let table =
            !view.all_inputs && name_width + id_width + path_width + 4 <= self.options.width;
        if table {
            self.heading(&format!(
                "{:<name_width$}  {:<id_width$}  PATH",
                "NAME", "SELECTOR"
            ))?;
            for (name, id, path) in &rows {
                let name = truncate_str(name, name_width, "...");
                let padding = " ".repeat(name_width.saturating_sub(measure_text_width(&name)));
                writeln!(
                    self.writer,
                    "{}{}  {}{}  {path}",
                    self.styled(&name, Style::new().bold()),
                    padding,
                    self.styled(id, Style::new().cyan()),
                    " ".repeat(id_width.saturating_sub(measure_text_width(id)))
                )?;
            }
        } else {
            for ((name, id, path), device) in rows.iter().zip(&view.devices) {
                writeln!(
                    self.writer,
                    "{}",
                    self.styled(
                        &truncate_str(name, self.options.width.max(12), "..."),
                        Style::new().bold().cyan()
                    )
                )?;
                // Selectors are kept whole and unadorned for copying, even on a narrower physical terminal.
                writeln!(self.writer, "  {}", self.styled(id, Style::new().cyan()))?;
                if view.all_inputs {
                    self.label("Class", device.class.name())?;
                    self.label(
                        "Placement",
                        &format!(
                            "origin {}; location {}",
                            device.origin.name(),
                            device.location.name()
                        ),
                    )?;
                }
                self.label("Path", path)?;
            }
        }
        writeln!(
            self.writer,
            "Rows are input attachments, not verified chassis; (+N) counts extra endpoints."
        )?;
        self.selector_notes(&view.devices)?;
        if self.options.detail {
            for device in &view.devices {
                writeln!(self.writer)?;
                self.inspect(device, None)?;
            }
        }
        Ok(())
    }
    fn selector_notes(&mut self, devices: &[DeviceView]) -> io::Result<()> {
        if !devices.is_empty() {
            writeln!(
                self.writer,
                "Readable handles are inventory-relative; --verbose shows qualified aliases and identity evidence."
            )?;
        }
        if devices
            .iter()
            .any(|d| d.user_selector.stability == "attachment_scoped")
        {
            writeln!(
                self.writer,
                "--at- qualifiers follow attachment topology; persistence across reconnects is not guaranteed."
            )?;
        }
        if devices.iter().any(|d| d.user_selector.ambiguous) {
            writeln!(
                self.writer,
                "Ambiguous selectors: inspect identity evidence and locators; matching never selects the first device."
            )?;
        }
        if devices.iter().any(|d| d.grouping == "uncertain_attachment") {
            writeln!(
                self.writer,
                "Some attachment membership is uncertain; inspect for endpoint evidence."
            )?;
        }
        Ok(())
    }
    fn device_heading(&mut self, device: &DeviceView) -> io::Result<()> {
        let name = safe(&device_name(device, false));
        let combined = format!("{name} ({})", device.user_selector.id);
        if measure_text_width(&combined) <= self.options.width {
            writeln!(
                self.writer,
                "{} ({})",
                self.styled(&name, Style::new().bold()),
                self.styled(&device.user_selector.id, Style::new().cyan())
            )?;
        } else {
            writeln!(
                self.writer,
                "{}",
                self.styled(
                    &truncate_str(&name, self.options.width.max(12), "..."),
                    Style::new().bold().cyan()
                )
            )?;
            writeln!(
                self.writer,
                "  {}",
                self.styled(&device.user_selector.id, Style::new().cyan())
            )?;
        }
        Ok(())
    }
    fn indicators(&mut self, view: &View) -> io::Result<()> {
        let devices: Vec<_> = view
            .devices
            .iter()
            .filter(|d| d.has_indicators() || view.selected)
            .collect();
        if devices.is_empty() {
            return writeln!(
                self.writer,
                "No recognized keyboard indicator endpoints{}; use `device list --all` for other inputs.",
                if matches!(view.discovery.inventory, InventoryStatus::Current) {
                    " found"
                } else {
                    " available; discovery is uncertain"
                }
            );
        }
        for (index, device) in devices.iter().enumerate() {
            if index > 0 {
                writeln!(self.writer)?;
            }
            self.device_heading(device)?;
            if device.origin == InputOrigin::Virtual || device.location != InputLocation::Unknown {
                self.label(
                    "Placement",
                    &format!(
                        "origin {}; location {}",
                        device.origin.name(),
                        device.location.name()
                    ),
                )?;
            }
            if device.endpoints.len() > 1 || device.grouping == "uncertain_attachment" {
                self.label(
                    "Scope",
                    &format!(
                        "{}; {} endpoints, chassis unverified",
                        device.grouping.replace('_', " "),
                        device.endpoints.len()
                    ),
                )?;
            }
            for (lock, label) in locks() {
                let summary = device.lock(lock);
                let word = self.state(summary.observation);
                let reasons = if matches!(
                    summary.observation.state,
                    LockState::Mixed | LockState::Disagreement
                ) {
                    format!(
                        " ({})",
                        summary
                            .contributing
                            .iter()
                            .map(|o| o.label())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                } else {
                    String::new()
                };
                writeln!(self.writer, "  {label:<12} {word}{reasons}")?;
            }
            if self.options.detail {
                for endpoint in device
                    .endpoints
                    .iter()
                    .filter(|e| e.applicable || e.host.is_some())
                {
                    self.indicator_endpoint(endpoint)?;
                }
            }
        }
        if view.devices.iter().any(|d| {
            d.lock(HostLock::Caps).contributing.len() > 1
                || d.lock(HostLock::Num).contributing.len() > 1
                || d.lock(HostLock::Scroll).contributing.len() > 1
        }) && !self.options.detail
        {
            writeln!(self.writer, "Use --verbose for endpoint values.")?;
        }
        self.selector_notes(&view.devices)?;
        Ok(())
    }
    fn state(&self, observation: LockObservation) -> String {
        let style = match observation.state {
            LockState::Known if observation.value == Some(true) => Style::new().green(),
            LockState::Known | LockState::Unsupported => Style::new().dim(),
            LockState::Error | LockState::PermissionDenied | LockState::Disagreement => {
                Style::new().red()
            }
            _ => Style::new().yellow(),
        };
        self.styled(observation.label(), style)
    }
    fn inspect(
        &mut self,
        device: &DeviceView,
        selected: Option<InputEndpointId>,
    ) -> io::Result<()> {
        self.device_heading(device)?;
        self.heading("Identity evidence")?;
        self.label("Display name", &device.name)?;
        self.label("User selector", &device.user_selector.id)?;
        self.label("Model slug", &device.user_selector.slug)?;
        self.label("Naming source", device.user_selector.name_source)?;
        self.label("Selector stability", device.user_selector.stability)?;
        self.label("Qualified alias", &device.user_selector.qualified)?;
        self.label(
            "Qualified stability",
            device.user_selector.qualified_stability,
        )?;
        self.label(
            "Disambiguation evidence",
            device.user_selector.disambiguation,
        )?;
        self.label(
            "Readable ambiguity",
            &device.user_selector.ambiguous.to_string(),
        )?;
        self.label(
            "Qualified ambiguity",
            &device.user_selector.qualified_ambiguous.to_string(),
        )?;
        self.heading("Technical identity aliases")?;
        self.label("Legacy evidence selector", &device.selector.id)?;
        self.label(
            "Evidence",
            &format!(
                "{}; {}; {}",
                device.selector.kind, device.selector.verification, device.selector.reason
            ),
        )?;
        self.label("Ambiguous", &device.selector.ambiguous.to_string())?;
        self.label("Attachment fallback", &device.selector.fallback)?;
        self.label(
            "Fallback ambiguous",
            &device.selector.fallback_ambiguous.to_string(),
        )?;
        self.metadata("USB IDs", &device.group.descriptor.usb, |ids| {
            format!("{:04x}:{:04x}", ids.vendor, ids.product)
        })?;
        self.metadata(
            "Advertised serial",
            &device.group.descriptor.serial,
            Clone::clone,
        )?;
        self.metadata(
            "USB product",
            &device.group.descriptor.product,
            Clone::clone,
        )?;
        self.label(
            "Registry model label",
            device
                .group
                .descriptor
                .model_label
                .as_deref()
                .unwrap_or("not identified"),
        )?;
        self.label(
            "Registry identification",
            &match &device.group.descriptor.identification {
                crate::model::identity::Identification::Unknown => "unknown".into(),
                crate::model::identity::Identification::Identified(model) => model.clone(),
                crate::model::identity::Identification::Ambiguous(models) => {
                    format!("ambiguous: {}", models.join(", "))
                }
            },
        )?;
        self.heading("Grouping and runtime identity")?;
        self.label("Input class", device.class.name())?;
        self.label("Origin", device.origin.name())?;
        self.label("Location", device.location.name())?;
        self.label(
            "Scope",
            &format!(
                "{}; not verified chassis identity",
                device.grouping.replace('_', " ")
            ),
        )?;
        self.label("Runtime group ID", &device.group.id.to_string())?;
        self.label(
            "Physical association",
            &device
                .group
                .physical
                .map(|p| p.to_string())
                .unwrap_or_else(|| "not established".into()),
        )?;
        self.label(
            "Sysfs locator",
            &device.group.descriptor.key.0.to_string_lossy(),
        )?;
        for endpoint in &device.endpoints {
            let e = &endpoint.endpoint;
            self.heading(&format!(
                "Endpoint {}{}",
                e.id,
                if Some(e.id) == selected {
                    " (selected)"
                } else {
                    ""
                }
            ))?;
            self.label("Sysfs locator", &e.descriptor.key.0.to_string_lossy())?;
            self.metadata("Event node", &e.descriptor.devnode, |p| {
                p.to_string_lossy().into_owned()
            })?;
            self.metadata("Name", &e.descriptor.name, Clone::clone)?;
            self.metadata("Physical path", &e.descriptor.physical_path, Clone::clone)?;
            self.metadata("Unique name", &e.descriptor.unique_name, Clone::clone)?;
            self.label("Group confirmed", &e.descriptor.group_confirmed.to_string())?;
            self.heading("Classification and placement evidence")?;
            self.label("Class", e.descriptor.classification.kind.name())?;
            self.label("Decision", e.descriptor.classification.reason.name())?;
            if let Some(c) = e.descriptor.classification.coverage {
                self.label(
                    "Typing coverage",
                    &format!(
                        "letters {}/26, digits {}/10, editing {}/4, modifier groups {}/3",
                        c.letters, c.digits, c.editing, c.modifiers
                    ),
                )?;
            }
            for (name, value) in &e.descriptor.input_properties {
                self.metadata(name, value, Clone::clone)?;
            }
            self.label("Origin", e.descriptor.placement.origin.name())?;
            self.label("Location", e.descriptor.placement.location.name())?;
            self.label("Placement reason", e.descriptor.placement.reason)?;
            for fact in &e.descriptor.placement.facts {
                self.metadata(&fact.path.to_string_lossy(), &fact.value, Clone::clone)?;
            }
            self.label("Stream access", "not checked by metadata discovery")?;
            self.heading("Kernel identifiers")?;
            for (label, value) in [
                ("Bus", &e.descriptor.identity.bus),
                ("Vendor", &e.descriptor.identity.vendor),
                ("Product", &e.descriptor.identity.product),
                ("Version", &e.descriptor.identity.version),
            ] {
                self.metadata(label, value, |v| format!("0x{v:04x}"))?;
            }
            self.heading("Capabilities")?;
            self.metadata("Event types", &e.descriptor.capabilities.events, codes)?;
            for (event, values) in &e.descriptor.capabilities.codes {
                self.metadata(&format!("Event {event} codes"), values, codes)?;
            }
            self.metadata(
                "Input properties",
                &e.descriptor.capabilities.properties,
                codes,
            )?;
            if endpoint.host.is_some() {
                self.indicator_endpoint(endpoint)?;
            }
        }
        Ok(())
    }
    fn indicator_endpoint(&mut self, endpoint: &EndpointView) -> io::Result<()> {
        let e = &endpoint.endpoint;
        self.heading(&format!(
            "  Endpoint {} ({})",
            e.id,
            e.descriptor
                .devnode
                .known()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| "no event locator".into())
        ))?;
        self.label("Sysfs locator", &e.descriptor.key.0.to_string_lossy())?;
        if let Some(host) = &endpoint.host {
            self.label(
                "Host source",
                &format!(
                    "{} generation {}; {}:{}",
                    host.id,
                    host.generation,
                    host.scope.namespace(),
                    host.scope.value()
                ),
            )?;
            self.label(
                "Availability",
                &match host.availability {
                    SourceAvailability::Active => "active".into(),
                    SourceAvailability::Unavailable(cause) => {
                        format!("unavailable ({})", super::failure(cause))
                    }
                },
            )?;
        }
        for (lock, label) in locks() {
            self.label(label, endpoint.observation(lock).label())?;
            if let Some(field) = endpoint.host.as_ref().and_then(|h| h.fields.get(&lock)) {
                if let Some(report) = &field.report {
                    self.label(
                        "Receipt",
                        &format!(
                            "sequence {}; received {} ms; fresh until {} ms",
                            report.stamp.sequence,
                            report.stamp.received_at.0.as_millis(),
                            report.stamp.fresh_until.0.as_millis()
                        ),
                    )?;
                    if let Validity::Stale { since, cause } = report.validity {
                        self.label(
                            "Stale",
                            &format!(
                                "since {} ms ({})",
                                since.0.as_millis(),
                                match cause {
                                    crate::model::state::StaleCause::Expired => "expired",
                                    crate::model::state::StaleCause::Replaced => "replaced",
                                    crate::model::state::StaleCause::Source(cause) =>
                                        super::failure(cause),
                                }
                            ),
                        )?;
                    }
                }
                if let Some(failure) = field.failure {
                    self.label(
                        "Failure",
                        &format!(
                            "{} at {} ms",
                            super::failure(failure.cause),
                            failure.at.0.as_millis()
                        ),
                    )?;
                }
                if let Some(sample) = &field.last_known {
                    self.label(
                        "Last known (historical)",
                        &format!(
                            "{} at {} ms, sequence {}",
                            if sample.value { "on" } else { "off" },
                            sample.stamp.received_at.0.as_millis(),
                            sample.stamp.sequence
                        ),
                    )?;
                }
            }
        }
        Ok(())
    }
    fn metadata<T>(
        &mut self,
        label: &str,
        value: &Metadata<T>,
        display: impl FnOnce(&T) -> String,
    ) -> io::Result<()> {
        match value {
            Metadata::Known(value) => self.label(label, &format!("{} [known]", display(value))),
            Metadata::Missing => self.label(label, "missing"),
            Metadata::Failed(problem) => {
                self.label(
                    label,
                    &format!("failed ({})", super::problem_kind(problem.kind)),
                )?;
                self.problem(problem)
            }
        }
    }
    fn problem(&mut self, problem: &Problem) -> io::Result<()> {
        self.label(
            "Metadata diagnostic",
            &format!(
                "{} at {}: {} (OS code {:?})",
                problem.operation,
                problem.path.display(),
                super::problem_kind(problem.kind),
                problem.os_code
            ),
        )
    }
}
fn locks() -> [(HostLock, &'static str); 3] {
    [
        (HostLock::Caps, "Caps Lock"),
        (HostLock::Num, "Num Lock"),
        (HostLock::Scroll, "Scroll Lock"),
    ]
}

fn device_name(device: &DeviceView, all: bool) -> String {
    let mut qualifiers = vec![];
    if all || device.class != InputClass::Keyboard {
        qualifiers.push(device.class.name().replace('_', " "));
    }
    if device.origin == InputOrigin::Virtual {
        qualifiers.push("virtual".into());
    } else if device.location != InputLocation::Unknown || all {
        qualifiers.push(device.location.name().into());
    }
    if qualifiers.is_empty() {
        device.name.clone()
    } else {
        format!("{} [{}]", device.name, qualifiers.join(", "))
    }
}
fn safe(text: &str) -> String {
    // Preserve Unicode labels while making ESC, newlines, tabs and bidi controls inert in terminal output.
    text.chars().flat_map(|c| c.escape_debug()).collect()
}
fn codes(values: &BTreeSet<u16>) -> String {
    if values.is_empty() {
        "none".into()
    } else {
        values
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    }
}
fn wrap(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut lines = vec![];
    let mut line = String::new();
    for word in text.split(' ') {
        if !line.is_empty() && measure_text_width(&line) + 1 + measure_text_width(word) > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests;
