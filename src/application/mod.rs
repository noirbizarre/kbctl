//! Shared direct/daemon operations; neither argument parsing nor terminal policy lives here.
// Portable operations remain testable where a real input host has not been implemented yet.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::{
    path::PathBuf,
    sync::Arc,
    task::{Context, Poll, Waker},
    time::Instant,
};

use crate::{
    error::Result,
    indicators::{self, Opener},
    input::{self, Source},
    model::{
        state::{Snapshot, StateError},
        time::Clock,
        topology::Topology,
    },
    output::{Document, Notice},
    registry::Registry,
    runtime::Runtime,
    state::{StateReader, subscription::ResyncReason},
};

pub(crate) mod devices;

#[derive(Clone, Debug)]
pub(crate) enum Operation {
    Devices,
    AllDevices,
    Inspect(PathBuf),
    Indicators { device: Option<String> },
    Watch,
    Status,
}
impl Operation {
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::Devices | Self::AllDevices => "device.list",
            Self::Inspect(_) => "device.inspect",
            Self::Indicators { .. } => "indicator.status",
            Self::Watch => "indicator.watch",
            Self::Status => "status",
        }
    }
    pub(crate) fn collects_indicators(&self) -> bool {
        matches!(self, Self::Indicators { .. } | Self::Watch | Self::Status)
    }
}

pub(crate) struct Application<S: Source, O: Opener> {
    pub(crate) runtime: Runtime,
    pub(crate) discovery: input::Service<S>,
    pub(crate) indicators: Option<indicators::Service<O>>,
    pub(crate) notices: Vec<Notice>,
}

impl<S: Source, O: Opener> Application<S, O> {
    pub(crate) fn new(
        source: S,
        opener: Option<O>,
        registry: Registry,
        clock: Arc<dyn Clock>,
    ) -> Result<Self> {
        let runtime = Runtime::new(Topology::default(), clock)?;
        let now = runtime.now();
        Ok(Self {
            runtime,
            discovery: input::Service::new(source, registry, input::Config::default(), now)?,
            indicators: opener
                .map(|opener| indicators::Service::new(opener, indicators::Config::default(), now))
                .transpose()?,
            notices: vec![],
        })
    }

    pub(crate) fn step(&mut self, ready: &[indicators::Ready]) -> Result<()> {
        self.notices.clear();
        let outcome = self.discovery.step(self.runtime.now(), &mut self.runtime)?;
        self.notices
            .extend(outcome.problems.iter().map(Notice::discovery));
        if let Some(service) = &mut self.indicators {
            let outcome = service.step(self.runtime.now(), ready, &mut self.runtime)?;
            self.notices
                .extend(outcome.problems.iter().map(Notice::indicator));
        } else {
            self.runtime.advance_time(self.runtime.now())?;
        }
        Ok(())
    }

    pub(crate) fn initialize(&mut self, operation: &Operation) -> Result<()> {
        // Resolve explicit scope from metadata before any collection; hidden devices still participate in identity ambiguity.
        let outcome = self.discovery.step(self.runtime.now(), &mut self.runtime)?;
        let initial_notices: Vec<_> = outcome.problems.iter().map(Notice::discovery).collect();
        if let Some(service) = &mut self.indicators {
            let scope = if let Operation::Indicators {
                device: Some(selector),
            } = operation
            {
                let catalog = devices::Catalog::new(&self.runtime.reader().snapshot()?);
                crate::input::classification::CollectionScope::Device(
                    catalog
                        .resolve(std::path::Path::new(selector), false)?
                        .device
                        .group
                        .id,
                )
            } else {
                crate::input::classification::CollectionScope::Keyboards
            };
            service.set_scope(scope);
        }
        self.step(&[])?;
        self.notices.extend(initial_notices);
        // One collector turn is budgeted. Cover the whole inventory, without waiting for failed-open retries.
        let rounds = self
            .runtime
            .input_endpoints()
            .count()
            .div_ceil(indicators::Config::default().endpoints);
        let mut notices = std::mem::take(&mut self.notices);
        for _ in 1..rounds {
            self.step(&[])?;
            notices.append(&mut self.notices);
        }
        self.notices = notices;
        Ok(())
    }

    pub(crate) fn deadline(&self) -> Instant {
        [
            Some(self.discovery.next_deadline()),
            self.indicators.as_ref().and_then(|s| s.next_deadline()),
            self.runtime.next_deadline(),
        ]
        .into_iter()
        .flatten()
        .min()
        .expect("discovery always has a deadline")
    }

    pub(crate) fn stop(&mut self) -> Result<()> {
        if let Some(service) = &mut self.indicators {
            service.stop(&mut self.runtime)?;
        }
        Ok(())
    }
}

/// An execution host owns readiness and cancellation; operations share the same published state boundary.
pub(crate) trait Host {
    fn initialize(&mut self, operation: &Operation) -> Result<()>;
    fn reader(&self) -> StateReader;
    fn notices(&self) -> &[Notice];
    /// False means cancellation; true means work was processed and the reader may have changed.
    fn turn(&mut self) -> Result<bool>;
    fn stop(&mut self) -> Result<()>;
}

pub(crate) fn execute(
    host: &mut impl Host,
    operation: &Operation,
    mut emit: impl FnMut(&Document) -> Result<()>,
) -> Result<()> {
    let result = execute_inner(host, operation, &mut emit);
    // stop closes every lease before attempting retirement, including when rendering/initialization failed.
    let cleanup = host.stop();
    result.and(cleanup)
}

fn execute_inner(
    host: &mut impl Host,
    operation: &Operation,
    emit: &mut impl FnMut(&Document) -> Result<()>,
) -> Result<()> {
    host.initialize(operation)?;
    let reader = host.reader();
    if !matches!(operation, Operation::Watch) {
        return emit(&Document::view(
            operation,
            "snapshot",
            &reader.snapshot()?,
            host.notices(),
        )?);
    }
    let (snapshot, mut subscription) = reader.subscribe_and_snapshot(64)?;
    let mut previous = Document::view(operation, "snapshot", &snapshot, host.notices())?;
    emit(&previous)?;
    while host.turn()? {
        let mut resync = false;
        // Publication is synchronous with turn(), so a no-op waker suffices: the host owns all wakeups.
        let mut context = Context::from_waker(Waker::noop());
        loop {
            match subscription.poll_next(&mut context) {
                Poll::Ready(Some(Ok(_))) => {}
                Poll::Ready(Some(Err(error))) if error.reason != ResyncReason::Closed => {
                    let (_, replacement) = reader.subscribe_and_snapshot(64)?;
                    subscription = replacement;
                    resync = true;
                    break;
                }
                Poll::Ready(Some(Err(_)) | None) => return Err(StateError::Closed.into()),
                Poll::Pending => break,
            }
        }
        let snapshot: Snapshot = reader.snapshot()?;
        let document = Document::view(
            operation,
            if resync { "resync" } else { "update" },
            &snapshot,
            host.notices(),
        )?;
        if resync || document.data != previous.data {
            emit(&document)?;
        }
        previous = document;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
