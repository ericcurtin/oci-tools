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

/// `ociman push` (unlike real `podman push`) always pushes back to
/// the exact reference an image is already stored under -- there is
/// no separate `DESTINATION` argument at all. An untagged image (0179)
/// has no such reference to push to in the first place: a real, clear
/// error, not a silent attempt to push to some nonsense destination
/// derived from this project's own internal sentinel string.
#[test]
fn push_of_an_untagged_image_is_a_clear_error() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/push-untagged-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        context_dir.path().join("Containerfile"),
        "FROM ociman-test/push-untagged-base:latest\nLABEL foo=bar\n",
    )
    .unwrap();

    let build = ociman(
        storage_dir.path(),
        &["build", context_dir.path().to_str().unwrap()],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let digest = String::from_utf8_lossy(&build.stdout)
        .lines()
        .next()
        .unwrap()
        .to_string();
    let short_id = &digest.trim_start_matches("sha256:")[..12];

    let push = ociman(storage_dir.path(), &["push", short_id]);
    assert!(!push.status.success());
    let stderr = String::from_utf8_lossy(&push.stderr);
    assert!(stderr.contains("cannot push an untagged image"), "{stderr}");
}
