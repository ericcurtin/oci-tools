//! Cross-binary integration tests for the oci-tools workspace.
//!
//! The actual tests live in `tests/tests/*.rs`. They exercise the built
//! binaries (`target/<profile>/<bin>`), so run them via a full workspace
//! invocation which builds all bin targets first:
//!
//! ```sh
//! cargo build --workspace && cargo test --workspace
//! ```
//!
//! Later milestones add lifecycle suites here: ociman build/run/exec
//! (rootless + root), ocirun runtime-spec conformance, the ocicri critest
//! subset, and the ociboot QEMU full-boot test.

use std::path::PathBuf;

/// Locate a workspace binary next to this test executable's target dir.
/// Shared by every file under `tests/tests/*.rs` so there is exactly one
/// implementation of "where did `cargo build --workspace` put the
/// binaries".
pub fn bin_path(name: &str) -> PathBuf {
    let mut path = std::env::current_exe().expect("current_exe");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.push(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    assert!(
        path.exists(),
        "binary {name} not found at {}; run `cargo build --workspace` first",
        path.display()
    );
    path
}
