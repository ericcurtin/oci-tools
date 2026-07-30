//! `ociman mount`/`ociman unmount` integration tests (`docs/design/
//! 0362`): a container's own real, already-directly-accessible root
//! filesystem path, and a real no-op respectively — for a running
//! container, a stopped one, an unknown one, and a rootless-overlay-
//! rootfs container being a clear error for `mount` (but never for
//! `unmount`, which has no such gap at all).
//!
//! Every test that needs a *plain*-rootfs container forces
//! `.rootless-overlay-supported` to `false` first (see
//! `ociman_diff.rs`'s own identical, already-established convention
//! and doc comment) — `mount_is_a_clear_error_for_a_rootless_overlay_
//! rootfs_container` below is the one test that deliberately leaves
//! it unset, written so it passes either way this host happens to
//! land.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use oci_spec_types::image::ContainerConfig;
use oci_store::Store;

use oci_tools_tests::{bin_path, busybox_path, seed_image};

fn ociman(storage_root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
        .env_remove("OCI_TOOLS_LOG")
        .args(args)
        .output()
        .expect("failed to spawn ociman")
}

/// A real, already-stopped container running `shell_command`, the
/// same technique `ociman_diff.rs`'s own `seed_and_run_stopped_
/// container` already established (forces plain-`Extract` rootfs
/// setup deterministically unless `force_extract` is `false`).
fn seed_and_run_stopped_container(
    storage_root: &Path,
    image: &str,
    shell_command: &str,
    force_extract: bool,
) -> String {
    if force_extract {
        std::fs::write(storage_root.join(".rootless-overlay-supported"), "false").unwrap();
    }
    let busybox = busybox_path().expect("busybox not found on $PATH");
    let store = Store::open(storage_root).unwrap();
    seed_image(
        &store,
        image,
        &busybox,
        &["sh"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                shell_command.to_string(),
            ]),
            ..Default::default()
        },
    );
    let run = ociman(storage_root, &["run", image]);
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let ps = ociman(storage_root, &["ps", "-a", "-q"]);
    let id = String::from_utf8_lossy(&ps.stdout).trim().to_string();
    assert!(!id.is_empty());
    id
}

#[test]
fn mount_prints_the_real_rootfs_path_of_a_stopped_container() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let id = seed_and_run_stopped_container(
        storage_dir.path(),
        "ociman-test/mount-stopped:latest",
        "exit 0",
        true,
    );

    let mount = ociman(storage_dir.path(), &["mount", &id]);
    assert!(
        mount.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&mount.stderr)
    );
    let printed = String::from_utf8_lossy(&mount.stdout).trim().to_string();
    let expected = storage_dir
        .path()
        .join("containers")
        .join(&id)
        .join("rootfs");
    assert_eq!(std::path::PathBuf::from(&printed), expected, "{mount:?}");
    assert!(
        Path::new(&printed).is_dir(),
        "the printed path should be a real, already-existing directory"
    );
}

#[test]
fn mount_works_on_a_genuinely_running_container_too() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        storage_dir.path().join(".rootless-overlay-supported"),
        "false",
    )
    .unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/mount-running:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let run = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args([
            "run",
            "-d",
            "ociman-test/mount-running:latest",
            "sleep",
            "30",
        ])
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn ociman run -d");
    assert!(run.status.success(), "{run:?}");
    let id = String::from_utf8_lossy(&ociman(storage_dir.path(), &["ps", "-a", "-q"]).stdout)
        .trim()
        .to_string();
    assert!(!id.is_empty());

    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let inspect = ociman(storage_dir.path(), &["inspect", &id, "--json"]);
        let json: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
        if json["status"] == "running" || Instant::now() >= deadline {
            assert_eq!(json["status"], "running", "{json:?}");
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    let mount = ociman(storage_dir.path(), &["mount", &id]);
    assert!(mount.status.success(), "{mount:?}");
    let printed = String::from_utf8_lossy(&mount.stdout).trim().to_string();
    assert!(Path::new(&printed).is_dir());

    let _ = ociman(storage_dir.path(), &["kill", &id]);
}

/// A real no-op: the container's own rootfs is fully intact
/// afterward, and `unmount` prints the container's own id, matching a
/// real installed `podman unmount`'s own checked-directly output.
#[test]
fn unmount_is_a_real_no_op_that_prints_the_container_id() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let id = seed_and_run_stopped_container(
        storage_dir.path(),
        "ociman-test/unmount-stopped:latest",
        "exit 0",
        true,
    );
    let rootfs = storage_dir
        .path()
        .join("containers")
        .join(&id)
        .join("rootfs");

    let unmount = ociman(storage_dir.path(), &["unmount", &id]);
    assert!(
        unmount.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&unmount.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&unmount.stdout).trim(), id);
    assert!(
        rootfs.is_dir(),
        "the container's own rootfs must survive unmount untouched"
    );
}

#[test]
fn mount_and_unmount_against_an_unknown_container_are_clear_errors() {
    let storage_dir = tempfile::tempdir().unwrap();

    let mount = ociman(storage_dir.path(), &["mount", "does-not-exist"]);
    assert!(!mount.status.success());

    let unmount = ociman(storage_dir.path(), &["unmount", "does-not-exist"]);
    assert!(!unmount.status.success());
}

/// Unlike `unmount` (never affected at all), `mount` shares `cp`/
/// `diff`/`export`/`commit`'s own real, checked-directly rootless-
/// overlay-rootfs gap (`docs/design/0146`) — a clear error, not a
/// silently wrong path.
#[test]
fn mount_is_a_clear_error_for_a_rootless_overlay_rootfs_container_but_unmount_still_succeeds() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    // Deliberately does *not* force the marker -- see the module's
    // own doc comment for why this test still passes either way.
    let id = seed_and_run_stopped_container(
        storage_dir.path(),
        "ociman-test/mount-overlay:latest",
        "exit 0",
        false,
    );

    let mount = ociman(storage_dir.path(), &["mount", &id]);
    let unmount = ociman(storage_dir.path(), &["unmount", &id]);

    let bundle_dir = storage_dir.path().join("containers").join(&id);
    if bundle_dir.join("upper").exists() {
        // This host really does support the rootless-overlay
        // optimization -- `mount` must refuse it clearly.
        assert!(!mount.status.success());
        assert!(
            String::from_utf8_lossy(&mount.stderr).contains("rootless-overlay"),
            "stderr: {}",
            String::from_utf8_lossy(&mount.stderr)
        );
    } else {
        // This host doesn't support it either -- plain `Extract` was
        // used, so `mount` succeeds normally.
        assert!(
            mount.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&mount.stderr)
        );
    }
    // `unmount` never has this gap at all, regardless of which branch
    // above actually ran on this host.
    assert!(
        unmount.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&unmount.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&unmount.stdout).trim(), id);
}
