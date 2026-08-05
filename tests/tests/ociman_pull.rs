//! `ociman pull` integration tests: the CLI-surface, no-real-network-
//! needed error path -- a real, end-to-end pull round trip against a
//! real mock registry is already covered by `ociman_tls_verify.rs`'s
//! own `MockRegistry` (`docs/design/0113`/`0307`); this file covers
//! only what fails before ever reaching the network at all.

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
fn pull_of_an_empty_reference_is_a_clear_error_before_any_network_attempt() {
    let storage_dir = tempfile::tempdir().unwrap();
    let pull = ociman(storage_dir.path(), &["pull", ""]);
    assert!(!pull.status.success());
    assert!(
        String::from_utf8_lossy(&pull.stderr).contains("image reference is empty"),
        "{}",
        String::from_utf8_lossy(&pull.stderr)
    );
}

/// `ociman image pull` (0481) is a real, genuine alias for `ociman
/// pull` itself, matching real `podman image pull`'s own checked-
/// directly identical `RunE`/flag set as top-level `podman pull`
/// exactly (`~/git/podman/cmd/podman/images/pull.go`) -- the same
/// real, no-network-needed error path above.
#[test]
fn image_pull_is_a_byte_identical_alias_for_pull() {
    let storage_dir = tempfile::tempdir().unwrap();

    let pull = ociman(storage_dir.path(), &["pull", ""]);
    let alias = ociman(storage_dir.path(), &["image", "pull", ""]);
    assert!(!pull.status.success());
    assert!(!alias.status.success());
    assert_eq!(alias.stderr, pull.stderr);
}
