//! Metadata-only Linux adapter. No evdev file descriptor is acquired by discovery.
#![allow(dead_code)]

use crate::{
    input::{Drain, Source},
    model::input::*,
};
use std::{
    io,
    os::fd::{AsFd, BorrowedFd},
    path::{Path, PathBuf},
};
use udev::AsRaw;

mod metadata;
pub(crate) use metadata::{Collector, SystemMetadata};

pub(crate) struct LinuxSource {
    metadata: Collector<SystemMetadata>,
    monitor: Option<udev::MonitorSocket>,
    generation: u64,
}
impl LinuxSource {
    pub(crate) fn new() -> Self {
        Self {
            metadata: Collector::new(SystemMetadata, "/sys".into(), kernel_word_bits()),
            monitor: None,
            generation: 0,
        }
    }
    /// A host must discard this registration when its generation changes; raw FDs can be reused.
    pub(crate) fn registration(&self) -> Option<(u64, BorrowedFd<'_>)> {
        self.monitor
            .as_ref()
            .map(|monitor| (self.generation, monitor.as_fd()))
    }
    pub(crate) fn is_current_registration(&self, generation: u64) -> bool {
        self.monitor.is_some() && generation == self.generation
    }
}

fn kernel_word_bits() -> Option<u32> {
    // These published 64-bit process ABIs require 64-bit kernels. Pointer width alone would not establish this.
    if cfg!(any(target_arch = "x86_64", target_arch = "aarch64")) {
        Some(64)
    } else {
        None
    }
}

pub(super) fn problem(path: &Path, operation: &str, error: &io::Error) -> Problem {
    Problem {
        path: path.into(),
        operation: operation.into(),
        kind: if error.kind() == io::ErrorKind::PermissionDenied {
            ProblemKind::Permission
        } else {
            ProblemKind::Io
        },
        os_code: error.raw_os_error(),
    }
}
pub(super) fn invalid(path: &Path, operation: &str, kind: ProblemKind) -> Problem {
    Problem {
        path: path.into(),
        operation: operation.into(),
        kind,
        os_code: None,
    }
}

impl Source for LinuxSource {
    fn start_monitor(&mut self) -> Result<(), Problem> {
        self.monitor = None;
        let generation = self.generation.checked_add(1).ok_or_else(|| {
            invalid(
                Path::new("udev monitor"),
                "allocate monitor generation",
                ProblemKind::Monitor,
            )
        })?;
        let monitor = udev::MonitorBuilder::new()
            .and_then(|builder| builder.match_subsystem("input"))
            .and_then(|builder| builder.listen())
            .map_err(|error| {
                problem(Path::new("udev monitor"), "enable input monitoring", &error)
            })?;
        self.generation = generation;
        self.monitor = Some(monitor);
        Ok(())
    }
    fn drain(&mut self, budget: usize) -> Result<Drain, Problem> {
        let monitor = self.monitor.as_ref().ok_or_else(|| {
            invalid(
                Path::new("udev monitor"),
                "receive input notifications",
                ProblemKind::Monitor,
            )
        })?;
        let mut changed = false;
        for _ in 0..budget {
            match receive(monitor) {
                Ok(true) => changed = true,
                Ok(false) => {
                    return Ok(Drain {
                        changed,
                        pending: false,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    // A broken registration must never be offered to a host after this turn.
                    self.monitor = None;
                    return Err(problem(
                        Path::new("udev monitor"),
                        "receive input notifications; reconcile and reopen monitor",
                        &error,
                    ));
                }
            }
        }
        Ok(Drain {
            changed,
            pending: true,
        })
    }
    fn scan(&mut self) -> Scan {
        self.metadata.scan()
    }
}

fn receive(monitor: &udev::MonitorSocket) -> io::Result<bool> {
    // udev's Option iterator hides both overflow and receive errors. Capture errno immediately instead.
    errno::set_errno(errno::Errno(0));
    // SAFETY: the owned monitor outlives this nonblocking receive. A nonnull pointer transfers one device reference.
    let device = unsafe { udev::ffi::udev_monitor_receive_device(monitor.as_raw()) };
    let code = errno::errno().0;
    if !device.is_null() {
        // SAFETY: release exactly the reference returned above; notification payload never becomes authority.
        unsafe {
            udev::ffi::udev_device_unref(device);
        }
        return Ok(true);
    }
    empty_receive(code)
}

fn empty_receive(code: i32) -> io::Result<bool> {
    if code == 0 {
        // An ambiguous null is not evidence of monitor health; periodic reconciliation remains necessary too.
        return Err(io::Error::other(
            "udev receive returned no device and no errno",
        ));
    }
    let error = io::Error::from_raw_os_error(code);
    if error.kind() == io::ErrorKind::WouldBlock {
        Ok(false)
    } else {
        Err(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_null_notification_is_idle_only_when_the_native_error_explicitly_says_would_block() {
        assert!(!empty_receive(11).unwrap());
        assert_eq!(
            empty_receive(4).unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
        assert_eq!(empty_receive(105).unwrap_err().raw_os_error(), Some(105));
        assert!(empty_receive(0).is_err());
    }

    #[test]
    fn constructing_a_source_does_not_open_hardware_or_offer_a_stale_registration() {
        let source = LinuxSource::new();
        assert!(source.registration().is_none());
        assert!(!source.is_current_registration(0));
    }
}
