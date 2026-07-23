//! `ocibox ephemeral` integration tests: exercises the actual built
//! `ocibox` binary creating a disposable box under a real, random
//! name, running a command inside it, and always removing it again --
//! a pure composition of `create`/`enter`/`rm`, matching real
//! `distrobox ephemeral` exactly (see `bin/ocibox/src/main.rs`'s own
//! `Command::Ephemeral` doc comment for the citation).

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

fn seed_ephemeral_base(storage_dir: &Path, reference: &str) {
    let store = Store::open(storage_dir).unwrap();
    seed_image(
        &store,
        reference,
        &busybox_path().expect("busybox not found on $PATH"),
        &["sh", "cat", "echo"],
        ContainerConfig::default(),
    );
}

#[test]
fn ephemeral_runs_a_command_and_removes_the_box_afterward() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    seed_ephemeral_base(storage_dir.path(), "ocibox-test/ephemeral-base:latest");

    let out = ocibox(
        storage_dir.path(),
        &[
            "ephemeral",
            "--image",
            "ocibox-test/ephemeral-base:latest",
            "--",
            "/bin/echo",
            "hello-ephemeral",
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("hello-ephemeral"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );

    // No box left behind at all -- not under any name.
    assert!(
        !storage_dir.path().join("boxes").exists()
            || std::fs::read_dir(storage_dir.path().join("boxes"))
                .unwrap()
                .next()
                .is_none(),
        "ephemeral should leave no box behind"
    );
}

#[test]
fn ephemeral_forwards_the_containers_own_nonzero_exit_code() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    seed_ephemeral_base(storage_dir.path(), "ocibox-test/ephemeral-exit-base:latest");

    let out = ocibox(
        storage_dir.path(),
        &[
            "ephemeral",
            "--image",
            "ocibox-test/ephemeral-exit-base:latest",
            "--",
            "/bin/sh",
            "-c",
            "exit 7",
        ],
    );
    assert_eq!(
        out.status.code(),
        Some(7),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The box is still removed even though the command inside it
    // failed -- matching real `distrobox ephemeral`'s own identical
    // "always clean up" behavior.
    let boxes_dir = storage_dir.path().join("boxes");
    assert!(
        !boxes_dir.exists() || std::fs::read_dir(&boxes_dir).unwrap().next().is_none(),
        "ephemeral should remove its own box even after a nonzero exit"
    );
}

#[test]
fn ephemeral_defaults_to_a_shell_when_no_command_is_given() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    seed_ephemeral_base(
        storage_dir.path(),
        "ocibox-test/ephemeral-shell-base:latest",
    );

    let out = ocibox(
        storage_dir.path(),
        &[
            "ephemeral",
            "--image",
            "ocibox-test/ephemeral-shell-base:latest",
        ],
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Two separate `ephemeral` invocations against the same image get
/// two different, real, randomly-generated box names -- confirmed
/// indirectly: each call's own rootfs starts fresh (a file written in
/// one invocation is never visible in a separate one), unlike
/// `ocibox enter`'s own deliberate same-box persistence.
#[test]
fn ephemeral_invocations_never_share_state_with_each_other() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    seed_ephemeral_base(
        storage_dir.path(),
        "ocibox-test/ephemeral-fresh-base:latest",
    );

    let write = ocibox(
        storage_dir.path(),
        &[
            "ephemeral",
            "--image",
            "ocibox-test/ephemeral-fresh-base:latest",
            "--",
            "/bin/sh",
            "-c",
            "echo marker > /marker.txt",
        ],
    );
    assert!(write.status.success());

    let read = ocibox(
        storage_dir.path(),
        &[
            "ephemeral",
            "--image",
            "ocibox-test/ephemeral-fresh-base:latest",
            "--",
            "/bin/cat",
            "/marker.txt",
        ],
    );
    // A fresh box never has the previous ephemeral invocation's own
    // marker file -- `cat` on a nonexistent file fails.
    assert!(!read.status.success());
}

#[test]
fn ephemeral_of_an_unresolvable_image_is_a_clear_error_and_leaves_no_box_behind() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let out = ocibox(
        storage_dir.path(),
        &["ephemeral", "--image", "127.0.0.1:1/doesnotexist:latest"],
    );
    assert!(!out.status.success());
    let boxes_dir = storage_dir.path().join("boxes");
    assert!(
        !boxes_dir.exists() || std::fs::read_dir(&boxes_dir).unwrap().next().is_none(),
        "a failed ephemeral create should leave no box directory behind at all"
    );
}
