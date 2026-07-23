//! `ociboot status` integration tests: reads back exactly what
//! `ociboot build-image` writes via `origin::write` — its own real,
//! first reader (see `docs/design/0222`). Deliberately narrow: this
//! is `origin::write`'s read-side counterpart, not a full bootc-style
//! booted/staged/rollback report (that needs real BLS-entry-to-
//! deployment linkage this project doesn't have yet).

use std::path::Path;
use std::process::Command;

use oci_spec_types::image::ContainerConfig;
use oci_store::Store;

use oci_tools_tests::{bin_path, busybox_path, seed_image};

fn ociboot(storage_root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin_path("ociboot"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
        .env_remove("OCI_TOOLS_LOG")
        .args(args)
        .output()
        .expect("failed to spawn ociboot")
}

fn mkfs_erofs_available() -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path)
        .map(|dir| dir.join("mkfs.erofs"))
        .any(|p| p.is_file())
}

/// `status` on a real, just-built deployment image reports exactly
/// what `build-image` wrote, both in human-readable and `--json`
/// form.
#[test]
fn status_reports_a_real_build_images_own_origin_record() {
    if !mkfs_erofs_available() {
        eprintln!("skipping: mkfs.erofs not installed");
        return;
    }
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    let reference = "ociboot-test/status-reference:latest";
    seed_image(
        &store,
        reference,
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let output_dir = tempfile::tempdir().unwrap();
    let output_path = output_dir.path().join("deployment.erofs");
    let build = ociboot(
        storage_dir.path(),
        &[
            "build-image",
            reference,
            "--output",
            output_path.to_str().unwrap(),
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let normalized = oci_spec_types::Reference::parse(reference)
        .unwrap()
        .to_string();
    let record = store.resolve_image(&normalized).unwrap().unwrap();

    let status = ociboot(
        storage_dir.path(),
        &["status", output_path.to_str().unwrap()],
    );
    assert!(
        status.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    let stdout = String::from_utf8_lossy(&status.stdout).into_owned();
    assert!(stdout.contains(&normalized), "got: {stdout:?}");
    assert!(
        stdout.contains(&record.manifest_digest.to_string()),
        "got: {stdout:?}"
    );
    assert!(stdout.contains("<none>"), "got: {stdout:?}");

    let status_json = ociboot(
        storage_dir.path(),
        &["--json", "status", output_path.to_str().unwrap()],
    );
    assert!(
        status_json.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&status_json.stderr)
    );
    let json: serde_json::Value =
        serde_json::from_slice(&status_json.stdout).expect("valid JSON on stdout");
    assert_eq!(json["image_reference"], normalized);
    assert_eq!(
        json["image_digest"].as_str().unwrap(),
        record.manifest_digest.to_string()
    );
    assert!(json["image_version"].is_null());
    assert!(json["built_at"].is_u64());
}

/// A path with no matching `.origin.json` sidecar (never built by
/// `ociboot build-image`, or a stray, unrelated file) is a clear,
/// real error naming the exact path — never a silent empty report or
/// a confusing lower-level I/O error.
#[test]
fn status_of_a_path_with_no_origin_record_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let output_dir = tempfile::tempdir().unwrap();
    let missing_path = output_dir.path().join("never-built.erofs");

    let status = ociboot(
        storage_dir.path(),
        &["status", missing_path.to_str().unwrap()],
    );
    assert!(!status.status.success());
    let stderr = String::from_utf8_lossy(&status.stderr);
    assert!(
        stderr.contains("no deployment origin record found"),
        "got: {stderr}"
    );
    assert!(
        stderr.contains(missing_path.to_str().unwrap()),
        "got: {stderr}"
    );
}
