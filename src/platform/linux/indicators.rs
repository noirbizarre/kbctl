//! Narrow evdev access: per-client filtering precedes every possible event read.
#![allow(dead_code)]
use super::ownership::OwnerGuard;
use crate::{
    indicators::{AccessError, Drain, ErrorKind, Fallback, Lease, Mode, Opener, Snapshot},
    model::{
        input::{Incarnation, InputEndpoint},
        state::{HostUnavailable, HostValue},
    },
};
use nix::{
    fcntl::{OFlag, open},
    sys::stat::{Mode as FileMode, SFlag, fstat, major, minor},
};
use std::{
    fs, io,
    os::{
        fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd},
        unix::fs::MetadataExt,
    },
    path::{Path, PathBuf},
    sync::Arc,
};

const EV_LED: u64 = 1 << 17;
const LOCKS: u64 = 0b111;
nix::ioctl_read_buf!(event_bits, b'E', 0x20, nix::libc::c_ulong);
nix::ioctl_read_buf!(led_bits, b'E', 0x20 + 17, nix::libc::c_ulong);
nix::ioctl_read_buf!(led_values, b'E', 0x19, nix::libc::c_ulong);
nix::ioctl_write_ptr!(event_mask, b'E', 0x93, nix::libc::input_mask);
nix::ioctl_write_ptr!(event_clock, b'E', 0xa0, nix::libc::c_int);

fn error(path: &Path, operation: &'static str, source: io::Error) -> AccessError {
    let kind = match source.raw_os_error() {
        Some(nix::libc::EACCES | nix::libc::EPERM) => ErrorKind::Permission,
        Some(nix::libc::ENOTTY | nix::libc::EOPNOTSUPP | nix::libc::EINVAL) => {
            ErrorKind::Unsupported
        }
        _ => ErrorKind::Inaccessible,
    };
    AccessError {
        path: path.into(),
        operation,
        kind,
        source: Some(source),
    }
}
fn invalid(path: &Path, operation: &'static str, kind: ErrorKind) -> AccessError {
    AccessError {
        path: path.into(),
        operation,
        kind,
        source: None,
    }
}

pub(crate) struct LinuxOpener {
    owner: Arc<OwnerGuard>,
}
impl LinuxOpener {
    pub(crate) fn new(owner: Arc<OwnerGuard>) -> Self {
        Self { owner }
    }
}
pub(crate) struct SystemIo {
    // Field order ensures the endpoint closes before the final ownership reference is released.
    fd: OwnedFd,
    _owner: Arc<OwnerGuard>,
    path: PathBuf,
}
impl Opener for LinuxOpener {
    type Lease = Driver<SystemIo>;
    fn open(&mut self, endpoint: &InputEndpoint) -> Result<Self::Lease, AccessError> {
        let descriptor = &endpoint.descriptor;
        let path = descriptor.devnode.known().ok_or_else(|| {
            invalid(
                &descriptor.key.0,
                "resolve discovered indicator node",
                ErrorKind::Binding,
            )
        })?;
        let expected = descriptor.incarnation.as_ref().ok_or_else(|| {
            invalid(
                path,
                "establish current sysfs incarnation",
                ErrorKind::Binding,
            )
        })?;
        check_incarnation(&descriptor.key.0, expected)?;
        let fd = open(
            path,
            // A replaced node must not become a controlling terminal before post-open identity checks.
            OFlag::O_RDONLY
                | OFlag::O_NONBLOCK
                | OFlag::O_NOFOLLOW
                | OFlag::O_CLOEXEC
                | OFlag::O_NOCTTY,
            FileMode::empty(),
        )
        .map_err(|e| error(path, "open read-only indicator node", e.into()))?;
        let stat =
            fstat(&fd).map_err(|e| error(path, "inspect opened indicator node", e.into()))?;
        if SFlag::from_bits_truncate(stat.st_mode) != SFlag::S_IFCHR {
            return Err(invalid(
                path,
                "validate character device",
                ErrorKind::Binding,
            ));
        }
        let link = PathBuf::from(format!(
            "/sys/dev/char/{}:{}",
            major(stat.st_rdev),
            minor(stat.st_rdev)
        ));
        let actual =
            fs::canonicalize(&link).map_err(|e| error(&link, "resolve opened device number", e))?;
        if actual != descriptor.key.0 {
            return Err(invalid(
                path,
                "match opened device to discovery",
                ErrorKind::Binding,
            ));
        }
        let driver = Driver::initialize(
            SystemIo {
                fd,
                _owner: self.owner.clone(),
                path: path.clone(),
            },
            event_kernel_supported(),
        )?;
        // Bracket admission, not only path lookup: replacement during filters/ioctls must not publish a mixed binding.
        check_incarnation(&descriptor.key.0, expected)?;
        if fs::canonicalize(&link).map_err(|e| error(&link, "revalidate opened attachment", e))?
            != descriptor.key.0
        {
            return Err(invalid(
                path,
                "revalidate indicator attachment",
                ErrorKind::Binding,
            ));
        }
        Ok(driver)
    }
}
fn check_incarnation(path: &Path, expected: &Incarnation) -> Result<(), AccessError> {
    let stat = fs::metadata(path).map_err(|e| error(path, "validate sysfs attachment", e))?;
    if (stat.dev(), stat.ino()) != (expected.device, expected.inode) {
        return Err(invalid(
            path,
            "reject replaced sysfs attachment",
            ErrorKind::Binding,
        ));
    }
    Ok(())
}
fn event_kernel_supported() -> bool {
    fs::read_to_string("/proc/sys/kernel/osrelease")
        .ok()
        .is_some_and(|release| compatible_release(&release))
}
fn compatible_release(release: &str) -> bool {
    let mut parts = release.trim().split('.');
    let major = parts.next().and_then(|part| part.parse::<u32>().ok());
    let minor = parts
        .next()
        .and_then(|part| part.split('-').next())
        .and_then(|part| part.parse::<u32>().ok());
    matches!((major,minor),(Some(major),Some(minor)) if (major,minor)>=(4,4))
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Event {
    Led,
    Report,
    Dropped,
    OtherSync,
}
/// This seam models syscalls, not key collection; tests provide an independent kernel-side queue.
pub(crate) trait Evdev {
    fn support(&mut self) -> Result<u64, AccessError>;
    fn values(&mut self) -> Result<u64, AccessError>;
    fn mask(&mut self, kind: u32, bits: u64) -> Result<(), AccessError>;
    fn clock(&mut self, clock: i32) -> Result<(), AccessError>;
    fn events(&mut self, budget: usize) -> Result<Vec<Event>, AccessError>;
}
pub(crate) struct Driver<I> {
    io: I,
    mode: Mode,
    initial: Option<Snapshot>,
    recovering: bool,
    warning: Option<AccessError>,
}
impl<I: Evdev> Driver<I> {
    fn initialize(mut io: I, compatible: bool) -> Result<Self, AccessError> {
        let support = io.support()? & LOCKS;
        let mut warning = None;
        let mode = if support == 0 {
            Mode::Unsupported
        } else if !compatible {
            Mode::Snapshots(Fallback::Kernel)
        } else {
            // Audited upstream Linux >=4.4: masks filter prospectively; an actual clock change purges ALL old queue types.
            // This is evdev_set_clk_type's implementation contract, not a generic queue-purge ioctl.
            // See 0c3e99437a66e4c869c60c2398449e6d98f3a988 and 06a16293f71927f756dcf37558a79c0b05a91641.
            let setup = io
                .mask(0, 1 | EV_LED)
                .and_then(|_| io.mask(17, LOCKS))
                .and_then(|_| io.clock(nix::libc::CLOCK_REALTIME))
                .and_then(|_| io.clock(nix::libc::CLOCK_MONOTONIC));
            match setup {
                Ok(()) => Mode::Events,
                Err(e) if matches!(e.kind, ErrorKind::Unsupported | ErrorKind::Permission) => {
                    warning = Some(e);
                    Mode::Snapshots(Fallback::Setup)
                }
                Err(e) => return Err(e),
            }
        };
        let active = if support == 0 { 0 } else { io.values()? };
        Ok(Self {
            io,
            mode,
            initial: Some(snapshot(support, active)),
            recovering: false,
            warning,
        })
    }
}
impl<I: Evdev> Lease for Driver<I> {
    fn mode(&self) -> Mode {
        self.mode
    }
    fn snapshot(&mut self) -> Result<Snapshot, AccessError> {
        if let Some(value) = self.initial.take() {
            return Ok(value);
        }
        let support = self.io.support()? & LOCKS;
        if support == 0 {
            self.mode = Mode::Unsupported;
        } else if self.mode == Mode::Unsupported {
            self.mode = Mode::Snapshots(Fallback::Setup);
        }
        Ok(snapshot(
            support,
            if support == 0 { 0 } else { self.io.values()? },
        ))
    }
    fn drain(&mut self, budget: usize) -> Result<Drain, AccessError> {
        let budget = budget.min(64);
        if self.mode != Mode::Events || budget == 0 {
            return Ok(Drain::default());
        }
        let events = match self.io.events(budget) {
            Ok(events) => events,
            Err(e)
                if e.source
                    .as_ref()
                    .is_some_and(|e| e.kind() == io::ErrorKind::Interrupted) =>
            {
                return Ok(Drain {
                    pending: true,
                    ..Drain::default()
                });
            }
            Err(e) => return Err(e),
        };
        let mut result = Drain {
            processed: events.len(),
            pending: events.len() == budget,
            ..Drain::default()
        };
        for event in events {
            match event {
                Event::Dropped => {
                    self.recovering = true;
                    result.gap = true;
                    result.changed = false;
                }
                Event::Report if self.recovering => {
                    self.recovering = false;
                    result.synchronized = true;
                    result.changed = true;
                    // Ignore the rest of this already-read batch; none of it may be replayed after the next snapshot.
                    break;
                }
                Event::Led if !self.recovering => result.changed = true,
                _ => {}
            }
        }
        Ok(result)
    }
    fn downgrade(&mut self, reason: Fallback) {
        self.mode = Mode::Snapshots(reason);
        self.recovering = false;
    }
    fn take_warning(&mut self) -> Option<AccessError> {
        self.warning.take()
    }
}
fn snapshot(support: u64, active: u64) -> Snapshot {
    let value = |bit: u32| {
        if support & (1u64 << bit) == 0u64 {
            HostValue::Unavailable(HostUnavailable::Unsupported)
        } else {
            HostValue::Known(active & (1u64 << bit) != 0u64)
        }
    };
    Snapshot {
        caps: value(1),
        num: value(0),
        scroll: value(2),
    }
}
impl Driver<SystemIo> {
    /// Readiness borrowing only: a host must not duplicate/reconfigure this FD outside its lease/ownership lifetime.
    pub(crate) fn registration(&self) -> Option<BorrowedFd<'_>> {
        (self.mode == Mode::Events).then(|| self.io.fd.as_fd())
    }
}
impl SystemIo {
    fn bitmap(
        &self,
        operation: &'static str,
        request: unsafe fn(i32, &mut [nix::libc::c_ulong]) -> nix::Result<i32>,
    ) -> Result<u64, AccessError> {
        let mut bits = [0 as nix::libc::c_ulong; 1];
        // SAFETY: this initialized, aligned word is writable for the entire request; wrappers encode its exact length.
        let count = unsafe { request(self.fd.as_raw_fd(), &mut bits) }
            .map_err(|e| error(&self.path, operation, e.into()))?;
        if count as usize != std::mem::size_of_val(&bits) {
            return Err(invalid(
                &self.path,
                "decode complete native indicator bitmap",
                ErrorKind::Protocol,
            ));
        }
        // c_ulong is 32-bit on other Linux ABIs; normalize only after decoding the native word.
        #[allow(clippy::unnecessary_cast)]
        let value = bits[0] as u64;
        Ok(value)
    }
}
impl Evdev for SystemIo {
    fn support(&mut self) -> Result<u64, AccessError> {
        if self.bitmap("query supported event types", event_bits)? & EV_LED == 0 {
            return Ok(0);
        }
        self.bitmap("query supported indicator codes", led_bits)
    }
    fn values(&mut self) -> Result<u64, AccessError> {
        self.bitmap("query current host indicator state", led_values)
    }
    fn mask(&mut self, kind: u32, bits: u64) -> Result<(), AccessError> {
        let bits = [bits as nix::libc::c_ulong];
        let mask = nix::libc::input_mask {
            type_: kind,
            codes_size: std::mem::size_of_val(&bits) as u32,
            codes_ptr: bits.as_ptr() as usize as u64,
        };
        // SAFETY: both the mask descriptor and its aligned native bitmap remain alive across this synchronous ioctl.
        unsafe { event_mask(self.fd.as_raw_fd(), &mask) }.map_err(|e| {
            error(
                &self.path,
                "install per-client LED notification mask",
                e.into(),
            )
        })?;
        Ok(())
    }
    fn clock(&mut self, clock: i32) -> Result<(), AccessError> {
        // SAFETY: EVIOCSCLOCKID takes a POINTER to c_int, not a clock ID passed as the variadic integer argument.
        unsafe { event_clock(self.fd.as_raw_fd(), &clock) }
            .map_err(|e| error(&self.path, "select per-client notification clock", e.into()))?;
        Ok(())
    }
    fn events(&mut self, budget: usize) -> Result<Vec<Event>, AccessError> {
        let count = budget.min(64);
        // SAFETY: Linux input_event consists only of integer fields; zero is a valid initialized representation.
        let mut events: Vec<nix::libc::input_event> =
            (0..count).map(|_| unsafe { std::mem::zeroed() }).collect();
        // SAFETY: expose exactly this initialized allocation's bytes while no typed references alias the syscall buffer.
        let buffer = unsafe {
            std::slice::from_raw_parts_mut(
                events.as_mut_ptr().cast::<u8>(),
                std::mem::size_of_val(events.as_slice()),
            )
        };
        let size = match nix::unistd::read(&self.fd, buffer) {
            Ok(0) => {
                return Err(invalid(
                    &self.path,
                    "read live indicator notifications",
                    ErrorKind::Inaccessible,
                ));
            }
            Ok(size) => size,
            Err(nix::errno::Errno::EAGAIN) => return Ok(vec![]),
            Err(e) => return Err(error(&self.path, "read indicator notifications", e.into())),
        };
        if size % std::mem::size_of::<nix::libc::input_event>() != 0 {
            return Err(invalid(
                &self.path,
                "decode complete indicator notification",
                ErrorKind::Protocol,
            ));
        }
        events.truncate(size / std::mem::size_of::<nix::libc::input_event>());
        events
            .into_iter()
            .map(|event| match (event.type_, event.code) {
                (17, 0..=2) => Ok(Event::Led),
                (0, 0) => Ok(Event::Report),
                (0, 3) => Ok(Event::Dropped),
                (0, _) => Ok(Event::OtherSync),
                // Never include unexpected codes, payloads or timestamps in diagnostics.
                _ => Err(invalid(
                    &self.path,
                    "validate filtered indicator notification classes",
                    ErrorKind::Protocol,
                )),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
