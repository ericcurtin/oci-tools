//! `ociman diff` integration tests (see `docs/design/0149`): a real
//! listing of every path that differs between a container's own
//! current filesystem and the base image it was created from, for
//! plain-text and `--json` output, a stopped container, an unknown
//! container, and a rootless-overlay-rootfs container being a clear
//! error too.
//!
//! Every test here forces `.rootless-overlay-supported` to `false`
//! (see `rootfs_setup::rootless_overlay_supported_cached`'s own doc
//! comment) *before* the container's first `run`, so the container
//! under test deterministically uses the plain `RootfsSetup::Extract`
//! layout `diff` actually supports, regardless of whether this
//! particular host happens to support the rootless-overlay
//! optimization or not — `diff_is_a_clear_error_for_a_rootless_
//! overlay_rootfs_container` below is the one test that deliberately
//! leaves it unset, to exercise the *other* branch for real (and is
//! written so it still passes either way: if this host doesn't
//! support the optimization either, `diff` just succeeds instead,
//! which is also a correct, passing outcome for that one test).

use std::path::Path;
use std::process::Command;

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

/// A real, already-stopped container (`diff` must work exactly as
/// well as on a running one, matching real `podman diff`) running
/// `shell_command`. Forces plain-`Extract` rootfs setup
/// deterministically first (see the module's own doc comment) unless
/// `force_extract` is `false`.
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
fn diff_reports_added_and_deleted_paths_and_never_shows_an_untouched_base_image_file() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let id = seed_and_run_stopped_container(
        storage_dir.path(),
        "ociman-test/diff-basic:latest",
        "echo hi > /new-file.txt; rm /bin/sh",
        true,
    );

    let diff = ociman(storage_dir.path(), &["diff", &id]);
    assert!(
        diff.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&diff.stderr)
    );
    let stdout = String::from_utf8_lossy(&diff.stdout);
    assert!(stdout.contains("A /new-file.txt"), "stdout: {stdout:?}");
    assert!(stdout.contains("D /bin/sh"), "stdout: {stdout:?}");
    // The real point of persisting a base snapshot rather than
    // re-extracting the base image fresh at diff time (0149's own doc
    // comment): an untouched base-image file (busybox's own real
    // binary, still hardlinked from every other applet) must never
    // show up as a false "changed" entry just because it was
    // extracted at a different wall-clock moment than the container's
    // own copy.
    assert!(
        !stdout.contains("busybox"),
        "an untouched base-image file must never appear in the diff: {stdout:?}"
    );
    // The synthesized `/etc/hosts` (docs/design/0147) is captured as
    // part of the container's own *base* state (written before the
    // base snapshot itself), so it never shows up as a diff entry
    // either, matching real docker/podman's own hiding of it.
    assert!(
        !stdout.contains("hosts"),
        "the synthesized /etc/hosts must never appear in the diff: {stdout:?}"
    );
}

#[test]
fn diff_json_reports_the_same_three_arrays_real_podman_diff_format_json_uses() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let id = seed_and_run_stopped_container(
        storage_dir.path(),
        "ociman-test/diff-json:latest",
        "echo hi > /new-file.txt; rm /bin/sh",
        true,
    );

    let diff = ociman(storage_dir.path(), &["diff", &id, "--json"]);
    assert!(
        diff.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&diff.stderr)
    );
    let view: serde_json::Value = serde_json::from_slice(&diff.stdout).unwrap();
    let added = view["added"].as_array().unwrap();
    assert!(
        added.iter().any(|v| v.as_str() == Some("/new-file.txt")),
        "added: {added:?}"
    );
    let deleted = view["deleted"].as_array().unwrap();
    assert!(
        deleted.iter().any(|v| v.as_str() == Some("/bin/sh")),
        "deleted: {deleted:?}"
    );
}

#[test]
fn diff_with_no_deliberate_changes_at_all_reports_no_base_image_files_as_changed() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let id = seed_and_run_stopped_container(
        storage_dir.path(),
        "ociman-test/diff-none:latest",
        "exit 0",
        true,
    );

    let diff = ociman(storage_dir.path(), &["diff", &id, "--json"]);
    assert!(
        diff.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&diff.stderr)
    );
    let view: serde_json::Value = serde_json::from_slice(&diff.stdout).unwrap();
    // The container's own runtime creates real, empty mount-point
    // directories for /dev, /proc, /sys before mounting over them --
    // a real, pre-existing (and correct: real docker/podman's own
    // `diff` shows these too) part of the added set, not something
    // this test should treat as a false positive. Nothing from the
    // base image itself (busybox, its own applets, /etc, ...) should
    // ever appear anywhere in the report.
    // Each field is omitted entirely (not an empty array) when empty
    // -- matches real podman's own `ChangesReportJSON`'s own
    // `omitempty` tags exactly.
    let empty = Vec::new();
    let all_paths: Vec<&str> = ["changed", "added", "deleted"]
        .iter()
        .flat_map(|key| view[key].as_array().unwrap_or(&empty).iter())
        .map(|v| v.as_str().unwrap())
        .collect();
    for path in &all_paths {
        assert!(
            matches!(*path, "/dev" | "/proc" | "/sys"),
            "unexpected diff entry for a container with no deliberate changes: {path:?} \
             (full report: {all_paths:?})"
        );
    }
}

#[test]
fn diff_against_an_unknown_container_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let diff = ociman(storage_dir.path(), &["diff", "does-not-exist"]);
    assert!(!diff.status.success());
}

#[test]
fn diff_is_a_clear_error_for_a_rootless_overlay_rootfs_container() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    // Deliberately does *not* force the marker -- see the module's
    // own doc comment for why this test still passes either way.
    let id = seed_and_run_stopped_container(
        storage_dir.path(),
        "ociman-test/diff-overlay:latest",
        "exit 0",
        false,
    );

    let diff = ociman(storage_dir.path(), &["diff", &id]);

    let bundle_dir = storage_dir.path().join("containers").join(&id);
    if bundle_dir.join("upper").exists() {
        // This host really does support the rootless-overlay
        // optimization -- `diff` must refuse it clearly.
        assert!(!diff.status.success());
        assert!(
            String::from_utf8_lossy(&diff.stderr).contains("rootless-overlay"),
            "stderr: {}",
            String::from_utf8_lossy(&diff.stderr)
        );
    } else {
        // This host doesn't support it either -- plain `Extract` was
        // used, so `diff` succeeds normally.
        assert!(
            diff.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&diff.stderr)
        );
    }
}
