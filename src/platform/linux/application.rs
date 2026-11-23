//! Synchronous direct-mode host: borrowed readiness registrations and a signal self-pipe.
use super::{indicators::LinuxOpener, input::LinuxSource, ownership::OwnerGuard};
use crate::{
    application::{Application, Host, Operation},
    error::{Error, Result},
    indicators::Ready,
    model::time::SystemClock,
    output::Notice,
    registry,
    state::StateReader,
};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use signal_hook::{
    SigId,
    consts::{SIGINT, SIGTERM},
    low_level,
};
use std::{
    os::{fd::AsFd, unix::net::UnixStream},
    sync::Arc,
};

struct Shutdown {
    read: UnixStream,
    handlers: Vec<SigId>,
}
impl Shutdown {
    fn new() -> Result<Self> {
        let (read, write) =
            UnixStream::pair().map_err(|e| Error::io("create shutdown wakeup", e))?;
        let mut shutdown = Self {
            read,
            handlers: vec![],
        };
        for signal in [SIGINT, SIGTERM] {
            let writer = write
                .try_clone()
                .map_err(|e| Error::io("clone shutdown wakeup", e))?;
            shutdown.handlers.push(
                low_level::pipe::register(signal, writer)
                    .map_err(|e| Error::io("register shutdown signal", e))?,
            );
        }
        Ok(shutdown)
    }
}
impl Drop for Shutdown {
    fn drop(&mut self) {
        for id in &self.handlers {
            low_level::unregister(*id);
        }
    }
}

pub(crate) struct LinuxHost {
    application: Application<LinuxSource, LinuxOpener>,
    // Keep the original writer alive in registered handlers until hosting finishes.
    shutdown: Shutdown,
}
impl LinuxHost {
    pub(crate) fn new(operation: &Operation) -> Result<Self> {
        let shutdown = Shutdown::new()?;
        let opener = if operation.collects_indicators() {
            Some(LinuxOpener::new(Arc::new(OwnerGuard::acquire()?)))
        } else {
            None
        };
        let application = Application::new(
            LinuxSource::new(),
            opener,
            registry::builtin::registry()?,
            Arc::new(SystemClock),
        )?;
        Ok(Self {
            application,
            shutdown,
        })
    }
}
impl Host for LinuxHost {
    fn initialize(&mut self, operation: &Operation) -> Result<()> {
        self.application.initialize(operation)
    }
    fn reader(&self) -> StateReader {
        self.application.runtime.reader()
    }
    fn notices(&self) -> &[Notice] {
        &self.application.notices
    }
    fn stop(&mut self) -> Result<()> {
        self.application.stop()
    }

    fn turn(&mut self) -> Result<bool> {
        let app = &self.application;
        let mut descriptors = vec![PollFd::new(self.shutdown.read.as_fd(), PollFlags::POLLIN)];
        let discovery = app.discovery.source().registration();
        if let Some((_, fd)) = discovery {
            descriptors.push(PollFd::new(fd, PollFlags::POLLIN));
        }
        let leases: Vec<_> = app
            .indicators
            .iter()
            .flat_map(|s| s.leases())
            .filter_map(|(endpoint, generation, lease)| {
                lease.registration().map(|fd| (endpoint, generation, fd))
            })
            .collect();
        for (_, _, fd) in &leases {
            descriptors.push(PollFd::new(*fd, PollFlags::POLLIN));
        }
        // Round up: truncating sub-millisecond deadlines would busy-spin until the clock catches up.
        let remaining = app.deadline().saturating_duration_since(app.runtime.now());
        let millis = remaining.as_millis().saturating_add(u128::from(
            !remaining.subsec_nanos().is_multiple_of(1_000_000),
        ));
        let timeout = PollTimeout::try_from(millis.min(i32::MAX as u128) as u64)
            .expect("bounded poll timeout");
        match poll(&mut descriptors, timeout) {
            Ok(_) => {}
            Err(nix::errno::Errno::EINTR) => return Ok(true),
            Err(error) => return Err(Error::io("wait for input readiness", error.into())),
        }
        if descriptors[0]
            .revents()
            .is_some_and(|flags| !flags.is_empty())
        {
            return Ok(false);
        }
        let offset = 1 + usize::from(discovery.is_some());
        let ready: Vec<_> = leases
            .iter()
            .zip(&descriptors[offset..])
            .filter_map(|((endpoint, generation, _), fd)| {
                let flags = fd.revents().unwrap_or_else(PollFlags::empty);
                (!flags.is_empty()).then_some(Ready {
                    endpoint: *endpoint,
                    generation: *generation,
                    closed: flags
                        .intersects(PollFlags::POLLHUP | PollFlags::POLLERR | PollFlags::POLLNVAL),
                })
            })
            .collect();
        // Drop borrowed registrations before step can close or replace their owning leases.
        drop(descriptors);
        if let Some((generation, _)) = discovery {
            debug_assert!(
                self.application
                    .discovery
                    .source()
                    .is_current_registration(generation)
            );
        }
        self.application.step(&ready)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_signals_wake_an_idle_poll_without_a_discovery_or_indicator_fd() {
        const CHILD: &str = "KBCTL_SHUTDOWN_TEST_CHILD";
        if std::env::var_os(CHILD).is_some() {
            // Isolate process-wide signal handlers from other concurrently running unit tests.
            let shutdown = Shutdown::new().unwrap();
            low_level::raise(SIGINT).unwrap();
            let mut descriptors = [PollFd::new(shutdown.read.as_fd(), PollFlags::POLLIN)];
            assert_eq!(poll(&mut descriptors, PollTimeout::ZERO).unwrap(), 1);
            assert!(
                descriptors[0]
                    .revents()
                    .unwrap()
                    .contains(PollFlags::POLLIN)
            );
            return;
        }
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "platform::linux::application::tests::shutdown_signals_wake_an_idle_poll_without_a_discovery_or_indicator_fd"])
            .env(CHILD, "1").status().unwrap();
        assert!(status.success());
    }
}
