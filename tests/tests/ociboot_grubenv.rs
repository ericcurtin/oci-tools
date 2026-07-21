//! `ociboot grubenv` integration tests: exercises the actual built
//! `ociboot` binary's own `create`/`list`/`set`/`unset` subcommands
//! against real files -- `oci_bls::grubenv` itself already has its
//! own thorough unit test coverage (including byte-for-byte
//! comparisons against the real `grub-editenv` binary), this is a
//! CLI-surface test on top of it.

use std::path::Path;
use std::process::Command;

use oci_tools_tests::bin_path;

fn ociboot_grubenv(file: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin_path("ociboot"))
        .arg("grubenv")
        .arg("--file")
        .arg(file)
        .args(args)
        .output()
        .expect("failed to spawn ociboot grubenv")
}

/// Locate a real, installed `grub-editenv`, or `None` if it isn't on
/// `$PATH` -- not installed by `ci/vm-prepare.sh`, so cross-compatibility
/// tests against it skip themselves (printing why, not failing) rather
/// than making it a hard CI dependency, the same established pattern
/// `busybox_path` already uses.
fn grub_editenv_path() -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("grub-editenv"))
        .find(|p| p.is_file())
}

#[test]
fn create_writes_a_real_1024_byte_blank_environment_block() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("grubenv");

    let out = ociboot_grubenv(&file, &["create"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let bytes = std::fs::read(&file).unwrap();
    assert_eq!(bytes.len(), 1024);
    assert!(bytes.starts_with(b"# GRUB Environment Block\n"));
}

#[test]
fn set_then_list_round_trips_real_values_in_insertion_order() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("grubenv");
    assert!(ociboot_grubenv(&file, &["create"]).status.success());

    let set = ociboot_grubenv(&file, &["set", "saved_entry=abc123", "boot_success=1"]);
    assert!(
        set.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&set.stderr)
    );

    let list = ociboot_grubenv(&file, &["list"]);
    assert!(list.status.success());
    assert_eq!(
        String::from_utf8_lossy(&list.stdout),
        "saved_entry=abc123\nboot_success=1\n"
    );
}

#[test]
fn set_replaces_an_existing_keys_value_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("grubenv");
    assert!(ociboot_grubenv(&file, &["create"]).status.success());
    assert!(
        ociboot_grubenv(&file, &["set", "a=1", "b=2"])
            .status
            .success()
    );
    assert!(
        ociboot_grubenv(&file, &["set", "a=updated"])
            .status
            .success()
    );

    let list = ociboot_grubenv(&file, &["list"]);
    assert!(list.status.success());
    // `a`'s own original position is preserved, not moved to the end.
    assert_eq!(String::from_utf8_lossy(&list.stdout), "a=updated\nb=2\n");
}

#[test]
fn unset_removes_a_variable() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("grubenv");
    assert!(ociboot_grubenv(&file, &["create"]).status.success());
    assert!(
        ociboot_grubenv(&file, &["set", "a=1", "b=2"])
            .status
            .success()
    );

    let unset = ociboot_grubenv(&file, &["unset", "a"]);
    assert!(
        unset.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&unset.stderr)
    );
    let list = ociboot_grubenv(&file, &["list"]);
    assert_eq!(String::from_utf8_lossy(&list.stdout), "b=2\n");
}

#[test]
fn set_with_no_equals_sign_is_a_clear_error() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("grubenv");
    assert!(ociboot_grubenv(&file, &["create"]).status.success());

    let out = ociboot_grubenv(&file, &["set", "NOEQUALSSIGN"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("invalid parameter"), "{stderr}");
}

#[test]
fn list_on_a_missing_file_is_a_real_surfaced_error() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("does-not-exist");

    let out = ociboot_grubenv(&file, &["list"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("reading"), "{stderr}");
}

/// Real, direct cross-compatibility check against the real
/// `grub-editenv` binary -- not just this project's own round trip.
#[test]
fn create_produces_a_file_byte_identical_to_the_real_grub_editenv() {
    let Some(grub_editenv) = grub_editenv_path() else {
        eprintln!("skipping: grub-editenv not found on $PATH");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let ours = dir.path().join("ours");
    let real = dir.path().join("real");

    assert!(ociboot_grubenv(&ours, &["create"]).status.success());
    let real_create = Command::new(&grub_editenv)
        .arg(&real)
        .arg("create")
        .output()
        .expect("failed to spawn grub-editenv");
    assert!(real_create.status.success());

    assert_eq!(std::fs::read(&ours).unwrap(), std::fs::read(&real).unwrap());
}

/// Same real cross-compatibility check, after a real `set` -- not
/// just the blank `create` case.
#[test]
fn set_produces_a_file_byte_identical_to_the_real_grub_editenv() {
    let Some(grub_editenv) = grub_editenv_path() else {
        eprintln!("skipping: grub-editenv not found on $PATH");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let ours = dir.path().join("ours");
    let real = dir.path().join("real");

    assert!(ociboot_grubenv(&ours, &["create"]).status.success());
    assert!(
        Command::new(&grub_editenv)
            .arg(&real)
            .arg("create")
            .status()
            .unwrap()
            .success()
    );

    assert!(
        ociboot_grubenv(&ours, &["set", "saved_entry=abc123", "boot_success=1"])
            .status
            .success()
    );
    assert!(
        Command::new(&grub_editenv)
            .arg(&real)
            .args(["set", "saved_entry=abc123", "boot_success=1"])
            .status()
            .unwrap()
            .success()
    );

    assert_eq!(std::fs::read(&ours).unwrap(), std::fs::read(&real).unwrap());
}
