//! `ociman history` integration tests: an image's own real layer
//! history, matching real `docker history`/`podman history` (see
//! `docs/design/0104`). Built via `ociman build` rather than
//! `seed_image` -- `seed_image`'s own synthetic fixture leaves
//! `ImageConfig.history` deliberately empty even though it has one
//! real layer (nothing in this project reads history off a bare
//! pulled image, until now), so a real, correctly-populated history
//! (mixing real new layers and metadata-only entries) needs a real
//! build, the same fully offline approach `ociman_build.rs` already
//! established.

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

#[test]
fn history_lists_real_layers_and_metadata_entries_newest_first() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/history-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/history-base:latest\n\
         RUN echo hello > /marker.txt\n\
         ENV FOO=bar\n",
    );
    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/history-result:latest",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let record = store
        .resolve_image("docker.io/ociman-test/history-result:latest")
        .unwrap()
        .unwrap();
    let manifest = store.image_manifest(&record).unwrap();
    let config = store.image_config(&record).unwrap();
    // The seeded base has no history entries of its own (see this
    // file's own module doc comment); the RUN layer plus the ENV
    // metadata-only entry make two total, one of them a real layer.
    assert_eq!(config.history.len(), 2);
    assert_eq!(manifest.layers.len(), 2);

    let history = ociman(
        storage_dir.path(),
        &["history", "ociman-test/history-result:latest", "--json"],
    );
    assert!(
        history.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&history.stderr)
    );
    let views: serde_json::Value = serde_json::from_slice(&history.stdout).unwrap();
    let rows = views.as_array().unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");

    // Newest first: ENV (metadata-only, size 0) comes before RUN.
    assert_eq!(rows[0]["created_by"], "ENV FOO=bar");
    assert_eq!(rows[0]["size"], 0);
    assert_eq!(
        rows[1]["created_by"],
        "RUN /bin/sh -c echo hello > /marker.txt"
    );
    assert_eq!(rows[1]["size"], manifest.layers[1].size);
    assert!(rows[1]["size"].as_u64().unwrap() > 0);

    // The table (non-JSON) form has a header and both rows too, with
    // the same newest-first order.
    let table = ociman(
        storage_dir.path(),
        &["history", "ociman-test/history-result:latest"],
    );
    assert!(table.status.success());
    let stdout = String::from_utf8_lossy(&table.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 3, "{stdout:?}");
    assert!(lines[0].contains("CREATED"), "{stdout:?}");
    assert!(lines[1].contains("ENV FOO=bar"), "{stdout:?}");
    assert!(lines[2].contains("RUN /bin/sh -c echo hello"), "{stdout:?}");
}

/// `history --no-trunc` (matching real `podman history --no-trunc`
/// exactly) shows the plain table's own `CREATED BY` column in full
/// instead of the default 60-character-plus-`...` truncation; has no
/// effect on `--format`/`--json`, which already show the full string
/// either way.
#[test]
fn history_no_trunc_shows_the_full_command_only_in_the_plain_table() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/history-no-trunc-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let long_command = "echo this-is-a-genuinely-long-shell-command-well-past-sixty-characters-total > /marker.txt";
    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        &format!("FROM ociman-test/history-no-trunc-base:latest\nRUN {long_command}\n"),
    );
    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/history-no-trunc-result:latest",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let full_created_by = format!("RUN /bin/sh -c {long_command}");
    assert!(
        full_created_by.chars().count() > 60,
        "fixture must actually exceed the 60-char truncation threshold"
    );

    // Without --no-trunc: truncated to "..." in the plain table.
    let truncated = ociman(
        storage_dir.path(),
        &["history", "ociman-test/history-no-trunc-result:latest"],
    );
    assert!(truncated.status.success());
    let truncated_stdout = String::from_utf8_lossy(&truncated.stdout).into_owned();
    assert!(truncated_stdout.contains("..."), "{truncated_stdout:?}");
    assert!(
        !truncated_stdout.contains(&full_created_by),
        "{truncated_stdout:?}"
    );

    // With --no-trunc: the full command appears verbatim, no "...".
    let full = ociman(
        storage_dir.path(),
        &[
            "history",
            "ociman-test/history-no-trunc-result:latest",
            "--no-trunc",
        ],
    );
    assert!(full.status.success());
    let full_stdout = String::from_utf8_lossy(&full.stdout).into_owned();
    assert!(full_stdout.contains(&full_created_by), "{full_stdout:?}");
    assert!(!full_stdout.contains("..."), "{full_stdout:?}");

    // --json already shows the full string either way, matching real
    // podman: --no-trunc only affects the plain table.
    let json = ociman(
        storage_dir.path(),
        &[
            "history",
            "ociman-test/history-no-trunc-result:latest",
            "--json",
        ],
    );
    assert!(json.status.success());
    let views: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(views[0]["created_by"], full_created_by);
}

#[test]
fn history_of_an_unknown_reference_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let history = ociman(
        storage_dir.path(),
        &["history", "ociman-test/never-pulled:latest"],
    );
    assert!(!history.status.success());
    assert!(
        String::from_utf8_lossy(&history.stderr).contains("no such image"),
        "{}",
        String::from_utf8_lossy(&history.stderr)
    );
}

#[test]
fn history_of_an_image_with_no_history_at_all_says_so() {
    // `seed_image`'s own bare fixture: a real layer, but (unlike a
    // real build) no `ImageConfig.history` entries at all -- exactly
    // the gap this test's own module doc comment explains.
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/history-empty:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let history = ociman(
        storage_dir.path(),
        &["history", "ociman-test/history-empty:latest"],
    );
    assert!(history.status.success());
    assert_eq!(
        String::from_utf8_lossy(&history.stdout).trim(),
        "no history"
    );

    let history_json = ociman(
        storage_dir.path(),
        &["history", "ociman-test/history-empty:latest", "--json"],
    );
    assert!(history_json.status.success());
    let views: serde_json::Value = serde_json::from_slice(&history_json.stdout).unwrap();
    assert!(views.as_array().unwrap().is_empty());
}

/// `history --format` (0338) renders one line per history entry,
/// newest first (same order the plain table/`--json` already use),
/// reusing the exact same Go-template-*lite* engine `ociman
/// inspect`/`ps`/`images`/`volume ls`/`info --format` (`0332`-`0337`)
/// already established.
#[test]
fn history_format_renders_one_line_per_entry_newest_first() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/history-format-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/history-format-base:latest\n\
         RUN echo hello > /marker.txt\n\
         ENV FOO=bar\n",
    );
    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/history-format-result:latest",
        ],
    );
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let format = ociman(
        storage_dir.path(),
        &[
            "history",
            "ociman-test/history-format-result:latest",
            "--format",
            "{{.created_by}}",
        ],
    );
    assert!(
        format.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&format.stderr)
    );
    let stdout = String::from_utf8_lossy(&format.stdout).into_owned();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[0], "ENV FOO=bar", "newest first: {lines:?}");
    assert_eq!(
        lines[1], "RUN /bin/sh -c echo hello > /marker.txt",
        "{lines:?}"
    );
}

/// `--format`, when given, takes priority over `--json`/the default
/// table, and an unresolvable field path is a real, immediate error --
/// same precedence and error behavior the whole `--format` family
/// already established.
#[test]
fn history_format_takes_priority_and_errors_on_an_unknown_field() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/history-format-priority:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/history-format-priority:latest\nRUN true\n",
    );
    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/history-format-priority-result:latest",
        ],
    );
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let format = ociman(
        storage_dir.path(),
        &[
            "history",
            "ociman-test/history-format-priority-result:latest",
            "--json",
            "--format",
            "{{.size}}",
        ],
    );
    assert!(format.status.success());
    assert!(
        String::from_utf8_lossy(&format.stdout)
            .trim()
            .parse::<u64>()
            .is_ok(),
        "the format template's own plain number, not --json's own array, should have won: {:?}",
        format.stdout
    );

    let bad = ociman(
        storage_dir.path(),
        &[
            "history",
            "ociman-test/history-format-priority-result:latest",
            "--format",
            "{{.nosuchfield}}",
        ],
    );
    assert!(!bad.status.success());
    assert!(
        String::from_utf8_lossy(&bad.stderr).contains("no field"),
        "{}",
        String::from_utf8_lossy(&bad.stderr)
    );
}

/// `ociman image history` (0480) is a real, genuine alias for
/// `ociman history` itself, matching real `podman image history`'s
/// own checked-directly identical `RunE`/flag set as top-level
/// `podman history` exactly (`~/git/podman/cmd/podman/images/
/// history.go`) -- byte-identical output for the same fixture state.
#[test]
fn image_history_is_a_byte_identical_alias_for_history() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/image-history-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        "FROM ociman-test/image-history-base:latest\n\
         RUN echo hello > /marker.txt\n\
         ENV FOO=bar\n",
    );
    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "ociman-test/image-history-result:latest",
        ],
    );
    assert!(build.status.success(), "{build:?}");

    let history = ociman(
        storage_dir.path(),
        &["history", "ociman-test/image-history-result:latest"],
    );
    assert!(history.status.success());

    let alias = ociman(
        storage_dir.path(),
        &[
            "image",
            "history",
            "ociman-test/image-history-result:latest",
        ],
    );
    assert!(
        alias.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&alias.stderr)
    );
    assert_eq!(alias.stdout, history.stdout);

    // The identical flag set works through the alias too.
    let alias_no_trunc = ociman(
        storage_dir.path(),
        &[
            "image",
            "history",
            "ociman-test/image-history-result:latest",
            "--no-trunc",
        ],
    );
    let history_no_trunc = ociman(
        storage_dir.path(),
        &[
            "history",
            "ociman-test/image-history-result:latest",
            "--no-trunc",
        ],
    );
    assert!(alias_no_trunc.status.success());
    assert_eq!(alias_no_trunc.stdout, history_no_trunc.stdout);
}
