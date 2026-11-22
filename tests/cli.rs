//! Executable-edge contracts that do not require discovery or physical input access.
use assert_cmd::Command;
use predicates::prelude::*;

fn kbctl() -> Command {
    Command::new(assert_cmd::cargo::cargo_bin!("kbctl"))
}

#[test]
fn help_exposes_the_read_only_command_groups_and_json_flag() {
    kbctl().arg("--help").assert().success().stdout(
        predicate::str::contains("device")
            .and(predicate::str::contains("indicator"))
            .and(predicate::str::contains("--json")),
    );
    kbctl()
        .args(["indicator", "watch", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("NDJSON"));
    kbctl()
        .args(["device", "inspect", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("/dev/input/event0"));
    kbctl()
        .args(["indicator", "status", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--device").and(predicate::str::contains("--verbose")));
}

#[test]
fn invalid_commands_and_missing_selectors_have_clap_exit_status() {
    for args in [
        vec!["indicator", "set"],
        vec!["device", "inspect"],
        vec!["run"],
    ] {
        kbctl()
            .args(args)
            .assert()
            .code(2)
            .stderr(predicate::str::contains("error:"));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn piped_json_setup_failures_are_compact_uncolored_and_actionable() {
    // Missing runtime storage fails before discovery or indicator hardware is touched.
    let output = kbctl()
        .args(["indicator", "status", "--json"])
        .env_remove("XDG_RUNTIME_DIR")
        .env("FORCE_COLOR", "1")
        .env("CLICOLOR_FORCE", "1")
        .assert()
        .code(1)
        .get_output()
        .clone();
    assert!(output.stderr.is_empty());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(!text.contains('\u{1b}'));
    assert_eq!(text.lines().count(), 1);
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["event"], "error");
    assert_eq!(value["error"]["code"], "kbctl::ownership::storage");
    assert!(
        value["error"]["help"]
            .as_str()
            .unwrap()
            .contains("XDG_RUNTIME_DIR")
    );
}

#[cfg(target_os = "linux")]
#[test]
fn human_setup_failures_are_diagnostics_on_stderr() {
    kbctl()
        .args(["indicator", "status"])
        .env_remove("XDG_RUNTIME_DIR")
        .env("NO_COLOR", "1")
        .assert()
        .code(1)
        .stdout("")
        .stderr(predicate::str::contains("XDG_RUNTIME_DIR"));
}

#[cfg(target_os = "linux")]
#[test]
fn a_running_owner_blocks_indicator_commands_before_discovery_or_hardware_opens() {
    use nix::fcntl::{Flock, FlockArg};
    use std::{
        fs::{self, OpenOptions},
        os::unix::fs::{OpenOptionsExt, PermissionsExt},
    };
    let directory = tempfile::tempdir().unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let private = directory.path().join("kbctl");
    fs::create_dir(&private).unwrap();
    fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(private.join("owner.lock"))
        .unwrap();
    let _owner = Flock::lock(file, FlockArg::LockExclusiveNonblock).unwrap();
    let output = kbctl()
        .args(["--json", "indicator", "watch"])
        .env("XDG_RUNTIME_DIR", directory.path())
        .assert()
        .code(1)
        .get_output()
        .clone();
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["error"]["code"], "kbctl::ownership::busy");
    assert!(output.stderr.is_empty());
}
