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

use std::path::{Path, PathBuf};
use std::process::Command;

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

/// Locate `busybox`, or `None` if it isn't installed. Every real
/// `ocirun` end-to-end test needs a minimal rootfs to `exec` something
/// in; `busybox` is present in this project's dev environment and
/// common on minimal cloud images, but isn't installed by `ci/
/// vm-prepare.sh`, so tests using it skip themselves — printing why, not
/// failing — when it isn't found, rather than making it a hard CI
/// dependency.
pub fn busybox_path() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("busybox"))
        .find(|p| p.is_file())
}

/// Build a minimal bundle at `dir`: a busybox-based rootfs with `sh` and
/// the given symlinked applets, and a rootless `config.json` running
/// `args` (a `/bin/sh -c "..."` style command is the expected shape).
pub fn write_bundle(dir: &Path, busybox: &Path, args: &[&str]) {
    let rootfs = dir.join("rootfs");
    std::fs::create_dir_all(rootfs.join("bin")).unwrap();
    std::fs::copy(busybox, rootfs.join("bin/busybox")).unwrap();
    for applet in ["sh", "echo", "true", "false"] {
        #[cfg(unix)]
        std::os::unix::fs::symlink("busybox", rootfs.join("bin").join(applet)).unwrap();
    }

    let out = Command::new(bin_path("ocirun"))
        .args(["spec", "--rootless", "--bundle"])
        .arg(dir)
        .output()
        .expect("failed to spawn ocirun spec");
    assert!(
        out.status.success(),
        "ocirun spec --rootless failed: {out:?}"
    );

    let config_path = dir.join("config.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["process"]["terminal"] = serde_json::json!(false);
    config["process"]["args"] = serde_json::json!(args);
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
}
