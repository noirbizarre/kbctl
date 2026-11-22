use super::*;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    process::Command,
};
fn directory() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    dir
}
#[test]
fn separate_open_descriptions_contend_and_releasing_ownership_keeps_the_lockfile_reusable() {
    let dir = directory();
    let guard = OwnerGuard::at(dir.path()).unwrap();
    assert!(matches!(
        OwnerGuard::at(dir.path()),
        Err(OwnershipError::Busy(_))
    ));
    let path = dir.path().join("kbctl/owner.lock");
    let before = fs::metadata(&path).unwrap();
    drop(guard);
    let _next = OwnerGuard::at(dir.path()).unwrap();
    use std::os::unix::fs::MetadataExt;
    assert_eq!(fs::metadata(path).unwrap().ino(), before.ino());
}
#[test]
fn a_competing_process_cannot_bypass_the_held_owner_lock() {
    const CHILD: &str = "KBCTL_OWNERSHIP_TEST_CHILD";
    if let Some(path) = std::env::var_os(CHILD) {
        assert!(matches!(
            OwnerGuard::at(Path::new(&path)),
            Err(OwnershipError::Busy(_))
        ));
        return;
    }
    let dir = directory();
    let _guard = OwnerGuard::at(dir.path()).unwrap();
    let result=Command::new(std::env::current_exe().unwrap()).args(["--exact","platform::linux::ownership::tests::a_competing_process_cannot_bypass_the_held_owner_lock"])
        .env(CHILD,dir.path()).status().unwrap();
    assert!(result.success());
}
#[test]
fn symlinks_and_nonprivate_storage_are_rejected_without_repairing_them() {
    let dir = directory();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(OwnerGuard::at(dir.path()).is_err());
    assert_eq!(
        fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
        0o755
    );
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let other = directory();
    symlink(other.path(), dir.path().join("kbctl")).unwrap();
    assert!(OwnerGuard::at(dir.path()).is_err());
    assert!(dir.path().join("kbctl").is_symlink());
}
#[test]
fn a_multilink_lockfile_is_rejected_before_any_lock_is_claimed() {
    let dir = directory();
    drop(OwnerGuard::at(dir.path()).unwrap());
    fs::hard_link(
        dir.path().join("kbctl/owner.lock"),
        dir.path().join("alias"),
    )
    .unwrap();
    assert!(matches!(
        OwnerGuard::at(dir.path()),
        Err(OwnershipError::Storage { .. })
    ));
}

#[test]
fn fifo_lockfiles_do_not_block_and_lockfile_symlinks_are_not_followed() {
    let dir = directory();
    drop(OwnerGuard::at(dir.path()).unwrap());
    let lock = dir.path().join("kbctl/owner.lock");
    fs::remove_file(&lock).unwrap();
    nix::unistd::mkfifo(&lock, Mode::from_bits_truncate(0o600)).unwrap();
    assert!(matches!(
        OwnerGuard::at(dir.path()),
        Err(OwnershipError::Storage { .. })
    ));
    fs::remove_file(&lock).unwrap();
    symlink(dir.path().join("target"), &lock).unwrap();
    assert!(OwnerGuard::at(dir.path()).is_err());
    assert!(!dir.path().join("target").exists());
}

#[test]
fn accessible_private_storage_still_requires_the_invoking_users_ownership() {
    let dir = directory();
    let guard = OwnerGuard::at(dir.path()).unwrap();
    let mut metadata = fstat(&guard._file).unwrap();
    assert!(private_owned(&metadata, 0o600));
    metadata.st_uid = if metadata.st_uid == 0 { 1 } else { 0 };
    assert!(!private_owned(&metadata, 0o600));
}
