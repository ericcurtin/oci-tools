//! `ociman image diff` integration tests (`docs/design/0573`): every
//! real path that differs between an image's own last layer and the
//! layer immediately beneath it — matching real `podman image diff
//! IMAGE`'s own single-positional case exactly (never the *whole*
//! image's cumulative content from scratch). Built via `ociman
//! build` rather than a hand-rolled synthetic multi-layer manifest —
//! the same "real build over a synthetic base" approach
//! `ociman_history.rs` already established, needed here for a real
//! second layer to diff against the first.

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

fn write_containerfile(dir: &Path, contents: &str) {
    std::fs::write(dir.join("Containerfile"), contents).unwrap();
}

/// A single-layer image diffs against a genuinely empty "before"
/// state — every real path in it shows as added, matching real
/// `podman image diff`'s own identical single-layer behavior
/// (live-verified directly against a real installed `podman 4.9.3`).
#[test]
fn image_diff_of_a_single_layer_image_reports_every_path_as_added() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/image-diff-single:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let diff = ociman(
        storage_dir.path(),
        &["image", "diff", "ociman-test/image-diff-single:latest"],
    );
    assert!(
        diff.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&diff.stderr)
    );
    let stdout = String::from_utf8_lossy(&diff.stdout);
    assert!(stdout.contains("A /bin/busybox"), "stdout: {stdout:?}");
    assert!(stdout.contains("A /bin/sh"), "stdout: {stdout:?}");
    // Not a single line is ever anything other than `A` for a
    // single-layer image -- there is no parent layer at all to have
    // deleted or modified anything relative to.
    for line in stdout.lines() {
        assert!(
            line.starts_with("A "),
            "unexpected non-added line: {line:?}"
        );
    }
}

/// The real point of this whole feature: a second, real layer's own
/// diff reports only *that* layer's own real changes -- never
/// anything from the base layer beneath it, matching real `podman
/// image diff`'s own identical "last layer vs. its own direct parent"
/// scope (not the image's full cumulative content).
#[test]
fn image_diff_of_a_multi_layer_image_reports_only_the_last_layers_own_changes() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/image-diff-base:latest",
        &busybox,
        &["sh", "true"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/image-diff-base:latest\n\
         RUN echo hello > /new-file.txt\n\
         ENV FOO=bar\n",
    );
    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/image-diff-result:latest",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let diff = ociman(
        storage_dir.path(),
        &["image", "diff", "ociman-test/image-diff-result:latest"],
    );
    assert!(
        diff.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&diff.stderr)
    );
    let stdout = String::from_utf8_lossy(&diff.stdout);
    assert!(stdout.contains("A /new-file.txt"), "stdout: {stdout:?}");
    // The base image's own real content (busybox itself, its own
    // applets) must never appear -- it's part of the "before" state
    // this diff is computed against, not a real change the last
    // layer itself introduced. `ENV` is metadata-only and adds no
    // layer/filesystem change of its own either.
    assert!(!stdout.contains("busybox"), "stdout: {stdout:?}");
    assert!(!stdout.contains("FOO"), "stdout: {stdout:?}");
    // Exactly one real line -- nothing else changed in this one new
    // layer.
    assert_eq!(stdout.lines().count(), 1, "stdout: {stdout:?}");
}

/// `--format json` renders the same three-array shape real `podman
/// image diff --format json` uses, identical to `ociman diff`'s own
/// already-established container convention.
#[test]
fn image_diff_format_json_matches_the_real_three_array_shape() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/image-diff-json:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let diff = ociman(
        storage_dir.path(),
        &[
            "image",
            "diff",
            "ociman-test/image-diff-json:latest",
            "--format",
            "json",
        ],
    );
    assert!(
        diff.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&diff.stderr)
    );
    let view: serde_json::Value = serde_json::from_slice(&diff.stdout).unwrap();
    assert!(view["added"].is_array());
    assert!(
        view["added"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("/bin/busybox"))
    );
    // `changed`/`deleted` are both entirely absent (never present-but-
    // empty), matching real podman's own `omitempty` shape -- nothing
    // was ever changed or deleted relative to a genuinely empty
    // "before" state.
    assert!(view.get("changed").is_none(), "{view:?}");
    assert!(view.get("deleted").is_none(), "{view:?}");
}

/// `--format` accepts only the literal `json`, matching `ociman
/// diff`'s own already-established real error wording exactly.
#[test]
fn image_diff_format_rejects_anything_other_than_json() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/image-diff-badformat:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let diff = ociman(
        storage_dir.path(),
        &[
            "image",
            "diff",
            "ociman-test/image-diff-badformat:latest",
            "--format",
            "{{.added}}",
        ],
    );
    assert!(!diff.status.success());
    assert!(
        String::from_utf8_lossy(&diff.stderr)
            .contains("only supported value for '--format' is 'json'"),
        "{}",
        String::from_utf8_lossy(&diff.stderr)
    );
}

/// Diffing an unknown image is a real, immediate error.
#[test]
fn image_diff_of_an_unknown_image_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let diff = ociman(
        storage_dir.path(),
        &["image", "diff", "ociman-test/does-not-exist:latest"],
    );
    assert!(!diff.status.success());
    assert!(
        String::from_utf8_lossy(&diff.stderr).contains("not known"),
        "{}",
        String::from_utf8_lossy(&diff.stderr)
    );
}

/// The global `--json` flag produces the exact same output as
/// `--format json`, matching every other `ociman` command's own
/// already-established convention.
#[test]
fn image_diff_global_json_flag_matches_format_json() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/image-diff-globaljson:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let via_format = ociman(
        storage_dir.path(),
        &[
            "image",
            "diff",
            "ociman-test/image-diff-globaljson:latest",
            "--format",
            "json",
        ],
    );
    let via_global = ociman(
        storage_dir.path(),
        &[
            "--json",
            "image",
            "diff",
            "ociman-test/image-diff-globaljson:latest",
        ],
    );
    assert!(via_format.status.success());
    assert!(via_global.status.success());
    assert_eq!(via_format.stdout, via_global.stdout);
}
