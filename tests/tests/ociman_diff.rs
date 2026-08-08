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

/// `ociman diff --format json` (`docs/design/0368`) produces the
/// exact same output as the global `--json` flag.
#[test]
fn diff_format_json_matches_the_global_json_flags_own_output() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let id = seed_and_run_stopped_container(
        storage_dir.path(),
        "ociman-test/diff-format-json:latest",
        "echo hi > /new-file.txt; rm /bin/sh",
        true,
    );

    let via_format = ociman(storage_dir.path(), &["diff", &id, "--format", "json"]);
    assert!(
        via_format.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&via_format.stderr)
    );
    let via_global_json = ociman(storage_dir.path(), &["diff", &id, "--json"]);
    assert!(via_global_json.status.success());
    assert_eq!(via_format.stdout, via_global_json.stdout);
}

/// `--format`, when given, wins outright over the global `--json`
/// flag even when they'd otherwise disagree — matching real podman's
/// own identical per-command-flag-over-global precedence.
#[test]
fn diff_format_json_wins_over_a_conflicting_global_json_false() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let id = seed_and_run_stopped_container(
        storage_dir.path(),
        "ociman-test/diff-format-json-wins:latest",
        "echo hi > /new-file.txt",
        true,
    );

    let diff = ociman(storage_dir.path(), &["diff", &id, "--format", "json"]);
    assert!(diff.status.success(), "{diff:?}");
    let view: serde_json::Value = serde_json::from_slice(&diff.stdout).unwrap();
    assert!(view.is_object(), "expected real JSON output: {view:?}");
}

/// Any `--format` value other than the literal `json` is a real,
/// immediate error — matching real `podman diff --format`'s own
/// checked-directly identical restriction exactly (it has no rich
/// Go-template engine at all for this specific command, unlike
/// `ociman ps`/`images`/`inspect --format`).
#[test]
fn diff_format_rejects_anything_other_than_json() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let id = seed_and_run_stopped_container(
        storage_dir.path(),
        "ociman-test/diff-format-invalid:latest",
        "exit 0",
        true,
    );

    let diff = ociman(storage_dir.path(), &["diff", &id, "--format", "{{.added}}"]);
    assert!(!diff.status.success());
    assert!(
        String::from_utf8_lossy(&diff.stderr)
            .contains("only supported value for '--format' is 'json'"),
        "{}",
        String::from_utf8_lossy(&diff.stderr)
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
    // directories for `/dev`/`/proc`/`/sys` before mounting over them
    // -- but real podman/docker never actually show these in a real
    // `diff`, live-verified directly against a real installed `podman
    // 4.9.3` (`~/git/podman/libpod/diff.go`'s own `initInodes` map,
    // unconditionally filtered out of every diff, checked directly --
    // this test previously asserted the *opposite*, a real, previously
    // -unnoticed bug fixed in `docs/design/0573`). Nothing at all
    // should appear in the report for a container with no deliberate
    // changes -- each field is omitted entirely (not an empty array)
    // when empty either way, matching real podman's own
    // `ChangesReportJSON`'s own `omitempty` tags exactly.
    let empty = Vec::new();
    let all_paths: Vec<&str> = ["changed", "added", "deleted"]
        .iter()
        .flat_map(|key| view[key].as_array().unwrap_or(&empty).iter())
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        all_paths.is_empty(),
        "unexpected diff entries for a container with no deliberate changes: {all_paths:?}"
    );
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

/// `ociman diff --latest`/`-l` (matching real `podman diff --latest`
/// exactly) shows the single, real most-recently-*created*
/// container's own diff -- an earlier container's own, genuinely
/// different change must never appear.
#[test]
fn diff_latest_shows_the_most_recently_created_containers_own_diff() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let older_id = seed_and_run_stopped_container(
        storage_dir.path(),
        "ociman-test/diff-latest-older:latest",
        "echo hi > /older-file.txt",
        true,
    );

    // A real, distinguishable creation-time gap.
    std::thread::sleep(std::time::Duration::from_secs(2));

    seed_and_run_stopped_container(
        storage_dir.path(),
        "ociman-test/diff-latest-newer:latest",
        "echo hi > /newer-file.txt",
        true,
    );

    let diff = ociman(storage_dir.path(), &["diff", "--latest"]);
    assert!(
        diff.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&diff.stderr)
    );
    let stdout = String::from_utf8_lossy(&diff.stdout);
    assert!(
        stdout.contains("A /newer-file.txt"),
        "--latest must show the most recently created container's own diff: {stdout:?}"
    );
    assert!(
        !stdout.contains("older-file.txt"),
        "--latest must never show an earlier container's own diff: {stdout:?}"
    );

    // Sanity: the explicit *older* id still resolves to its own,
    // genuinely different diff too (both containers are independent,
    // not accidentally merged).
    let explicit = ociman(storage_dir.path(), &["diff", &older_id]);
    assert!(
        explicit.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&explicit.stderr)
    );
    let explicit_stdout = String::from_utf8_lossy(&explicit.stdout);
    assert!(
        explicit_stdout.contains("A /older-file.txt"),
        "{explicit_stdout:?}"
    );
}

/// A real, deliberate divergence from every other sibling in this
/// rollout: real podman's own checked-directly `diffRun`/`Diff` has
/// no mutual-exclusivity check at all between `--latest` and an
/// explicit `ID` -- the explicit one always silently wins outright,
/// never a real error, ported faithfully here too.
#[test]
fn diff_explicit_id_silently_wins_over_latest_when_both_given() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let older_id = seed_and_run_stopped_container(
        storage_dir.path(),
        "ociman-test/diff-latest-explicit-wins-older:latest",
        "echo hi > /older-file.txt",
        true,
    );

    std::thread::sleep(std::time::Duration::from_secs(2));

    let _newer_id = seed_and_run_stopped_container(
        storage_dir.path(),
        "ociman-test/diff-latest-explicit-wins-newer:latest",
        "echo hi > /newer-file.txt",
        true,
    );

    // `--latest` would resolve to the *newer* container, but the
    // explicit `older_id` given alongside it must win instead.
    let diff = ociman(storage_dir.path(), &["diff", "--latest", &older_id]);
    assert!(
        diff.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&diff.stderr)
    );
    let stdout = String::from_utf8_lossy(&diff.stdout);
    assert!(
        stdout.contains("A /older-file.txt"),
        "the explicit id must win over --latest: {stdout:?}"
    );
    assert!(
        !stdout.contains("newer-file.txt"),
        "the explicit id must win over --latest: {stdout:?}"
    );
}

/// Neither `--latest` nor an explicit container at all is a real,
/// immediate error.
#[test]
fn diff_with_no_id_and_no_latest_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(storage_dir.path(), &["diff"]);
    assert!(!out.status.success());
}

/// `diff --latest` on a genuinely empty store is a real, clear error,
/// matching real `podman diff --latest`'s own `ErrNoSuchCtr`.
#[test]
fn diff_latest_on_an_empty_store_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    let out = ociman(storage_dir.path(), &["diff", "--latest"]);
    assert!(!out.status.success());
}
