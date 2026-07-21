//! `ociman push` integration tests: the CLI-surface, error-path
//! coverage that needs no real network at all -- `oci_registry::push`
//! (and the `Client::blob_exists`/`upload_blob`/`push_manifest`
//! primitives it's built on) already has its own thorough mock-
//! registry test coverage in `crates/oci-registry/src/push.rs`,
//! including a real, manually-verified end-to-end round trip against
//! a real local `registry:2` instance during this feature's own
//! development (`docs/design/0127`).

use std::path::Path;
use std::process::Command;

use oci_tools_tests::bin_path;

fn ociman(storage_root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
        .env_remove("OCI_TOOLS_LOG")
        .args(args)
        .output()
        .expect("failed to spawn ociman")
}

#[test]
fn push_of_an_unknown_reference_is_a_clear_error_before_any_network_attempt() {
    let storage_dir = tempfile::tempdir().unwrap();
    let push = ociman(
        storage_dir.path(),
        &["push", "ociman-test/never-pulled-or-built:latest"],
    );
    assert!(!push.status.success());
    assert!(
        String::from_utf8_lossy(&push.stderr).contains("no such image"),
        "{}",
        String::from_utf8_lossy(&push.stderr)
    );
}

#[test]
fn push_of_an_unknown_image_id_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let push = ociman(storage_dir.path(), &["push", "0123456789ab"]);
    assert!(!push.status.success());
    assert!(
        String::from_utf8_lossy(&push.stderr).contains("no such image"),
        "{}",
        String::from_utf8_lossy(&push.stderr)
    );
}
