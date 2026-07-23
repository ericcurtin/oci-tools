//! `ocibox enter` integration tests: exercises the actual built
//! `ocibox` binary launching a real container (via the same shared
//! `oci_runtime_core::launch`/`Bundle`/`validate` two-phase lifecycle
//! `ociman run`/`ocirun run` already use) inside an already-`create`d
//! box's own rootfs. Confirms: real exit-code forwarding (both success
//! and nonzero), default-shell detection when no `COMMAND` is given,
//! the box's own rootfs persisting a write across two separate `enter`
//! invocations (even though the container *process* itself does not,
//! see this project's own `Command::Enter` doc comment for why not
//! yet), and a clear error for an unknown box name.

use std::path::Path;
use std::process::Command;

use oci_spec_types::image::ContainerConfig;
use oci_store::Store;

use oci_tools_tests::{bin_path, busybox_path, seed_image};

fn ocibox(storage_root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin_path("ocibox"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
        .env_remove("OCI_TOOLS_LOG")
        .args(args)
        .output()
        .expect("failed to spawn ocibox")
}

/// Seeds a real busybox-based image and `create`s a box from it,
/// returning the storage dir (kept alive for the caller) and the
/// box's own name.
fn make_box(storage_dir: &tempfile::TempDir, name: &str) {
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ocibox-test/enter-base:latest",
        &busybox_path().expect("busybox not found on $PATH"),
        &["sh", "cat", "echo"],
        ContainerConfig::default(),
    );
    let create = ocibox(
        storage_dir.path(),
        &[
            "create",
            "--image",
            "ocibox-test/enter-base:latest",
            "--name",
            name,
        ],
    );
    assert!(
        create.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&create.stderr)
    );
}

#[test]
fn enter_runs_an_explicit_command_and_forwards_its_exit_code() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    make_box(&storage_dir, "testbox");

    let ok = ocibox(
        storage_dir.path(),
        &["enter", "testbox", "--", "/bin/sh", "-c", "exit 0"],
    );
    assert_eq!(
        ok.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&ok.stderr)
    );

    let failing = ocibox(
        storage_dir.path(),
        &["enter", "testbox", "--", "/bin/sh", "-c", "exit 42"],
    );
    assert_eq!(
        failing.status.code(),
        Some(42),
        "stderr: {}",
        String::from_utf8_lossy(&failing.stderr)
    );
}

#[test]
fn enter_defaults_to_a_shell_when_no_command_is_given() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    make_box(&storage_dir, "testbox");

    // Busybox's own seeded rootfs here has no `/bin/bash`, only
    // `/bin/sh` (via the `"sh"` applet symlink) -- confirms the
    // `/bin/bash`-then-`/bin/sh` fallback actually reaches `/bin/sh`
    // rather than failing outright.
    let out = ocibox(storage_dir.path(), &["enter", "testbox"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn enter_persists_rootfs_writes_across_separate_invocations() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    make_box(&storage_dir, "testbox");

    let write = ocibox(
        storage_dir.path(),
        &[
            "enter",
            "testbox",
            "--",
            "/bin/sh",
            "-c",
            "echo persisted-marker > /marker.txt",
        ],
    );
    assert!(
        write.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&write.stderr)
    );

    // A wholly separate `enter` invocation -- a fresh container
    // process each time (see this test module's own doc comment) --
    // still sees the file the first invocation wrote, since only the
    // *process* is per-invocation, not the box's own rootfs.
    let read = ocibox(
        storage_dir.path(),
        &["enter", "testbox", "--", "/bin/cat", "/marker.txt"],
    );
    assert!(
        read.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&read.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&read.stdout).trim(),
        "persisted-marker"
    );
}

#[test]
fn enter_bind_mounts_a_real_existing_home() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    make_box(&storage_dir, "testbox");
    let home_dir = tempfile::tempdir().unwrap();
    std::fs::write(home_dir.path().join("canary.txt"), b"real-host-home").unwrap();

    let out = Command::new(bin_path("ocibox"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .env("HOME", home_dir.path())
        .args([
            "enter",
            "testbox",
            "--",
            "/bin/cat",
            &format!("{}/canary.txt", home_dir.path().display()),
        ])
        .output()
        .expect("failed to spawn ocibox");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "real-host-home");
}

#[test]
fn enter_of_an_unknown_box_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let out = ocibox(storage_dir.path(), &["enter", "no-such-box"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no such box"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
