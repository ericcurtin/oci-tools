//! `ocibox export --bin` integration tests (`docs/design/0252`):
//! exercises the actual built `ocibox` binary writing (and removing)
//! a real wrapper script that routes an exported binary's own
//! invocations through `ocibox enter` — checked directly against real
//! `distrobox export --bin`'s own actual shell implementation
//! (`~/git/distrobox/internal/inside-distrobox/assets/distrobox-export`),
//! deliberately scoped to just the binary-export half (see
//! `Command::Export`'s own doc comment for exactly why not `--app`
//! yet, and how the explicit `--box` flag here diverges from real
//! `distrobox export`'s own "detect which box I'm running in" model).

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

/// Seeds a real busybox-based image and `create`s a box from it.
fn make_box(storage_dir: &tempfile::TempDir, name: &str) {
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ocibox-test/export-base:latest",
        &busybox_path().expect("busybox not found on $PATH"),
        &["sh", "echo"],
        ContainerConfig::default(),
    );
    let create = ocibox(
        storage_dir.path(),
        &[
            "create",
            "--image",
            "ocibox-test/export-base:latest",
            "--name",
            name,
        ],
    );
    assert!(
        create.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&create.stderr)
    );
}

#[test]
fn export_writes_a_real_executable_wrapper_that_actually_runs_the_binary() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    make_box(&storage_dir, "testbox");
    let export_dir = tempfile::tempdir().unwrap();

    let export = ocibox(
        storage_dir.path(),
        &[
            "export",
            "--box",
            "testbox",
            "--bin",
            "/bin/echo",
            "--export-path",
            export_dir.path().to_str().unwrap(),
        ],
    );
    assert!(
        export.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&export.stderr)
    );
    assert!(
        String::from_utf8_lossy(&export.stdout).contains("exported successfully"),
        "{}",
        String::from_utf8_lossy(&export.stdout)
    );

    let wrapper = export_dir.path().join("echo");
    assert!(wrapper.is_file(), "wrapper should exist at {wrapper:?}");
    let contents = std::fs::read_to_string(&wrapper).unwrap();
    assert!(contents.contains("ocibox_binary"), "{contents:?}");
    assert!(contents.contains("testbox"), "{contents:?}");
    assert!(contents.contains("/bin/echo"), "{contents:?}");

    // Real, executable, and actually runs the exported binary inside
    // the box via a real `ocibox enter` -- not just a plausible-
    // looking file. `ocibox` itself must resolve on $PATH here since
    // the wrapper's own `exec ocibox enter ...` line calls it by bare
    // name (matching real `distrobox-export`'s own identical
    // `${DISTROBOX_PATH:-"distrobox"}` convention); this test's own
    // build directory is prepended to $PATH for exactly that reason.
    let bin_dir = bin_path("ocibox").parent().unwrap().to_path_buf();
    let path_var = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let run = Command::new(&wrapper)
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .env("PATH", path_var)
        .arg("hello-from-wrapper")
        .output()
        .expect("failed to run the exported wrapper");
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        "hello-from-wrapper"
    );
}

#[test]
fn export_of_a_missing_binary_is_a_clear_error() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    make_box(&storage_dir, "testbox");
    let export_dir = tempfile::tempdir().unwrap();

    let out = ocibox(
        storage_dir.path(),
        &[
            "export",
            "--box",
            "testbox",
            "--bin",
            "/bin/does-not-exist",
            "--export-path",
            export_dir.path().to_str().unwrap(),
        ],
    );
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("cannot find"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !export_dir.path().join("does-not-exist").exists(),
        "a failed export must leave no wrapper behind"
    );
}

#[test]
fn export_of_an_unknown_box_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    let export_dir = tempfile::tempdir().unwrap();

    let out = ocibox(
        storage_dir.path(),
        &[
            "export",
            "--box",
            "no-such-box",
            "--bin",
            "/bin/echo",
            "--export-path",
            export_dir.path().to_str().unwrap(),
        ],
    );
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no such box"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn export_delete_removes_the_wrapper_and_refuses_a_foreign_file() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    make_box(&storage_dir, "testbox");
    let export_dir = tempfile::tempdir().unwrap();

    let export = ocibox(
        storage_dir.path(),
        &[
            "export",
            "--box",
            "testbox",
            "--bin",
            "/bin/echo",
            "--export-path",
            export_dir.path().to_str().unwrap(),
        ],
    );
    assert!(export.status.success());
    let wrapper = export_dir.path().join("echo");
    assert!(wrapper.is_file());

    // A file that was never `ocibox export`ed (no marker comment) is
    // refused, matching real `distrobox export --delete`'s own
    // identical safety check -- confirmed the foreign file survives
    // completely untouched.
    let foreign = export_dir.path().join("foreign");
    std::fs::write(&foreign, "#!/bin/sh\necho not an export\n").unwrap();
    let refuse = ocibox(
        storage_dir.path(),
        &[
            "export",
            "--box",
            "testbox",
            "--bin",
            "/bin/foreign",
            "--export-path",
            export_dir.path().to_str().unwrap(),
            "--delete",
        ],
    );
    assert!(!refuse.status.success());
    assert!(
        String::from_utf8_lossy(&refuse.stderr).contains("not an ocibox-exported binary"),
        "{}",
        String::from_utf8_lossy(&refuse.stderr)
    );
    assert!(foreign.is_file(), "the foreign file must survive untouched");

    // The real, genuinely-exported wrapper deletes cleanly.
    let delete = ocibox(
        storage_dir.path(),
        &[
            "export",
            "--box",
            "testbox",
            "--bin",
            "/bin/echo",
            "--export-path",
            export_dir.path().to_str().unwrap(),
            "--delete",
        ],
    );
    assert!(
        delete.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&delete.stderr)
    );
    assert!(
        String::from_utf8_lossy(&delete.stdout).contains("removed successfully"),
        "{}",
        String::from_utf8_lossy(&delete.stdout)
    );
    assert!(!wrapper.exists(), "the wrapper should really be gone now");
}

#[test]
fn export_rejects_a_non_absolute_bin_path() {
    if busybox_path().is_none() {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    make_box(&storage_dir, "testbox");
    let export_dir = tempfile::tempdir().unwrap();

    let out = ocibox(
        storage_dir.path(),
        &[
            "export",
            "--box",
            "testbox",
            "--bin",
            "relative/echo",
            "--export-path",
            export_dir.path().to_str().unwrap(),
        ],
    );
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("absolute path"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
