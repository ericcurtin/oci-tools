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
