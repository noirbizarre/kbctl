//! The shared per-user ownership contract; existence of the lockfile is never ownership.
#![allow(dead_code)]
use miette::Diagnostic;
use nix::{
    fcntl::{OFlag, open, openat},
    sys::stat::{Mode, SFlag, fstat, mkdirat},
    unistd::geteuid,
};
use std::{
    env, io,
    os::fd::{AsRawFd, OwnedFd},
    path::{Component, Path, PathBuf},
};
use thiserror::Error;

#[derive(Debug, Error, Diagnostic)]
pub(crate) enum OwnershipError {
    #[error("unsafe ownership storage at {}: {reason}", path.display())]
    #[diagnostic(
        code(kbctl::ownership::storage),
        help(
            "Set XDG_RUNTIME_DIR to an absolute, user-owned private directory (0700). Check the kbctl directory (0700) and regular, single-link owner.lock (0600); do not delete a live lockfile or use symlinks."
        )
    )]
    Storage { path: PathBuf, reason: &'static str },
    #[error("cannot {operation} at {}", path.display())]
    #[diagnostic(
        code(kbctl::ownership::io),
        help(
            "Check ownership and permissions of this runtime storage; run kbctl as your own user with a valid XDG_RUNTIME_DIR."
        )
    )]
    Io {
        path: PathBuf,
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("another kbctl process owns {}", .0.display())]
    #[diagnostic(
        code(kbctl::ownership::busy),
        help(
            "Use the existing owner or stop it before retrying. Never delete or replace its lockfile to bypass contention."
        )
    )]
    Busy(PathBuf),
}
pub(crate) struct OwnerGuard {
    _file: OwnedFd,
    _directory: OwnedFd,
}
fn unsafe_storage(path: &Path, reason: &'static str) -> OwnershipError {
    OwnershipError::Storage {
        path: path.into(),
        reason,
    }
}
fn io_error(path: &Path, operation: &'static str, error: nix::errno::Errno) -> OwnershipError {
    OwnershipError::Io {
        path: path.into(),
        operation,
        source: error.into(),
    }
}
impl OwnerGuard {
    #[cfg(test)]
    pub(crate) fn for_test(path: &Path) -> Result<Self, OwnershipError> {
        Self::at(path)
    }
    pub(crate) fn acquire() -> Result<Self, OwnershipError> {
        let path = env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .ok_or_else(|| {
                unsafe_storage(Path::new("XDG_RUNTIME_DIR"), "runtime directory is unset")
            })?;
        Self::at(&path)
    }
    fn at(path: &Path) -> Result<Self, OwnershipError> {
        if !path.is_absolute()
            || path
                .components()
                .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
        {
            return Err(unsafe_storage(
                path,
                "expected an absolute path without parent traversal",
            ));
        }
        let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
        let mut directory = open(Path::new("/"), flags, Mode::empty())
            .map_err(|e| io_error(path, "open runtime root", e))?;
        for part in path.components().skip(1) {
            directory = openat(&directory, part.as_os_str(), flags, Mode::empty())
                .map_err(|e| io_error(path, "walk runtime directory without symlinks", e))?;
            let metadata =
                fstat(&directory).map_err(|e| io_error(path, "inspect runtime ancestor", e))?;
            // Sticky root-owned ancestors permit normal /tmp fixtures without trusting arbitrary writable parents.
            let trusted_owner = metadata.st_uid == 0 || metadata.st_uid == geteuid().as_raw();
            let writable = metadata.st_mode & 0o022 != 0;
            let sticky_root = metadata.st_uid == 0 && metadata.st_mode & 0o1000 != 0;
            if !trusted_owner || (writable && !sticky_root) {
                return Err(unsafe_storage(path, "untrusted runtime ancestor"));
            }
        }
        check_private_dir(&directory, path)?;
        match mkdirat(&directory, "kbctl", Mode::from_bits_truncate(0o700)) {
            Ok(()) | Err(nix::errno::Errno::EEXIST) => {}
            Err(e) => return Err(io_error(path, "create private kbctl directory", e)),
        }
        let parent = directory;
        let directory = openat(&parent, "kbctl", flags, Mode::empty())
            .map_err(|e| io_error(path, "open private kbctl directory", e))?;
        let lock_path = path.join("kbctl/owner.lock");
        check_private_dir(&directory, &lock_path)?;
        let file = openat(
            &directory,
            "owner.lock",
            OFlag::O_RDWR
                | OFlag::O_CREAT
                | OFlag::O_NOFOLLOW
                | OFlag::O_NONBLOCK
                | OFlag::O_CLOEXEC,
            Mode::from_bits_truncate(0o600),
        )
        .map_err(|e| io_error(&lock_path, "open ownership file", e))?;
        let before = check_lock(&file, &lock_path)?;
        // SAFETY: the private owned FD is valid; flock changes only this open file description's advisory lock.
        // Closing the final FD releases it without a potentially panicking unlock destructor.
        let result =
            unsafe { nix::libc::flock(file.as_raw_fd(), nix::libc::LOCK_EX | nix::libc::LOCK_NB) };
        if let Err(e) = nix::errno::Errno::result(result) {
            return Err(if matches!(e, nix::errno::Errno::EAGAIN) {
                OwnershipError::Busy(lock_path)
            } else {
                io_error(&lock_path, "acquire ownership", e)
            });
        }
        let after = check_lock(&file, &lock_path)?;
        let named = openat(
            &directory,
            "owner.lock",
            OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| io_error(&lock_path, "revalidate lockfile path", e))?;
        if before != after || check_lock(&named, &lock_path)? != after {
            return Err(unsafe_storage(
                &lock_path,
                "lockfile changed during admission",
            ));
        }
        // Revalidate the named directory too; a renamed private directory must not split cooperating owners.
        let named_dir = openat(&parent, "kbctl", flags, Mode::empty())
            .map_err(|e| io_error(path, "revalidate ownership directory", e))?;
        let a = fstat(&directory).map_err(|e| io_error(path, "inspect ownership directory", e))?;
        let b = fstat(&named_dir)
            .map_err(|e| io_error(path, "inspect named ownership directory", e))?;
        if (a.st_dev, a.st_ino) != (b.st_dev, b.st_ino) {
            return Err(unsafe_storage(path, "ownership directory changed"));
        }
        Ok(Self {
            _file: file,
            _directory: directory,
        })
    }
}
fn check_private_dir(fd: &OwnedFd, path: &Path) -> Result<(), OwnershipError> {
    let stat = fstat(fd).map_err(|e| io_error(path, "validate private directory", e))?;
    if !private_owned(&stat, 0o700) {
        return Err(unsafe_storage(
            path,
            "directory must be user-owned and mode 0700",
        ));
    }
    Ok(())
}
fn check_lock(fd: &OwnedFd, path: &Path) -> Result<(u64, u64), OwnershipError> {
    let stat = fstat(fd).map_err(|e| io_error(path, "validate ownership file", e))?;
    if SFlag::from_bits_truncate(stat.st_mode) != SFlag::S_IFREG
        || !private_owned(&stat, 0o600)
        || stat.st_nlink != 1
    {
        return Err(unsafe_storage(
            path,
            "lockfile must be user-owned, regular, private and single-link",
        ));
    }
    Ok((stat.st_dev, stat.st_ino))
}

fn private_owned(stat: &nix::sys::stat::FileStat, mode: u32) -> bool {
    // A successful open does not prove invoking-user ownership when capabilities grant extra access.
    stat.st_uid == geteuid().as_raw() && stat.st_mode & 0o7777 == mode
}

#[cfg(test)]
mod tests;
