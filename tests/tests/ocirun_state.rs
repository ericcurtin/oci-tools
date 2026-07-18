//! `ocirun state`/`ocirun list` integration tests.
//!
//! `create` (the rest of milestone 3) doesn't exist yet, so there is no
//! way to populate a state store through the CLI itself; these tests
//! populate one directly via `oci_runtime_core::StateStore` (the same
//! crate `ocirun` links against) and then exercise the built binary
//! against it, proving the CLI wiring and the state model agree on the
//! same on-disk format.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use oci_runtime_core::StateStore;
use oci_tools_tests::bin_path;

fn ocirun(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin_path("ocirun"))
        .arg("--root")
        .arg(root)
        .args(args)
        .env_remove("OCI_TOOLS_LOG")
        .output()
        .expect("failed to spawn ocirun")
}

#[test]
fn list_on_empty_root_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("state");

    let out = ocirun(&root, &["list"]);
    assert!(out.status.success(), "ocirun list failed: {out:?}");
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("ID") && stdout.contains("STATUS"));
    assert_eq!(stdout.lines().count(), 1, "header only, no containers");

    let out = ocirun(&root, &["list", "--format", "json"]);
    assert!(out.status.success());
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json, serde_json::json!([]));
}

#[test]
fn state_reports_missing_container_as_a_loud_error() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("state");

    let out = ocirun(&root, &["state", "does-not-exist"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.starts_with("error: "), "got: {stderr:?}");
    assert!(stderr.contains("does not exist"), "got: {stderr:?}");
}

#[test]
fn state_and_list_report_a_container_created_via_the_shared_state_store() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("state");

    let store = StateStore::open(&root).unwrap();
    let mut annotations = BTreeMap::new();
    annotations.insert("com.example.test".to_string(), "yes".to_string());
    store
        .create(
            "my-container",
            Path::new("/bundle"),
            Path::new("/bundle/rootfs"),
            annotations,
        )
        .unwrap();

    let out = ocirun(&root, &["state", "my-container"]);
    assert!(out.status.success(), "ocirun state failed: {out:?}");
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["id"], "my-container");
    assert_eq!(json["status"], "creating");
    assert_eq!(json["bundle"], "/bundle");
    assert_eq!(json["rootfs"], "/bundle/rootfs");
    assert_eq!(json["annotations"]["com.example.test"], "yes");

    let out = ocirun(&root, &["list", "--quiet"]);
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(stdout.trim(), "my-container");

    let out = ocirun(&root, &["list", "--format", "json"]);
    assert!(out.status.success());
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json.as_array().unwrap().len(), 1);
    assert_eq!(json[0]["id"], "my-container");
}

#[test]
fn list_rejects_unknown_format() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("state");

    let out = ocirun(&root, &["list", "--format", "yaml"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.starts_with("error: "), "got: {stderr:?}");
}
