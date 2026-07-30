//! `ociman container` subcommand family integration tests
//! (`docs/design/0357`): currently just `exists` (see
//! `ociman_exists.rs`) and `prune`.
//!
//! `ociman container prune` removes every real, non-running container
//! (this project's own `Created`/`Stopped`, never `Running`/`Paused`,
//! and never `Creating` either) — matching real `podman container
//! prune`'s own identical eligibility filter exactly (checked
//! directly against `~/git/podman/libpod/runtime_ctr.go`'s own
//! `PruneContainers`).

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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

fn ociman_run_detached(storage_root: &Path, image: &str, container_args: &[&str]) {
    let out = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
        .env_remove("OCI_TOOLS_LOG")
        .args(["run", "-d", image])
        .args(container_args)
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn ociman run -d");
    assert!(
        out.status.success(),
        "ociman run -d failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn inspect_json(storage_root: &Path, id: &str) -> serde_json::Value {
    let out = ociman(storage_root, &["inspect", id, "--json"]);
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("inspect --json output was not valid JSON: {e}"))
}

fn wait_for_status(storage_root: &Path, id: &str, want: &str, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let status = inspect_json(storage_root, id)["status"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if status == want || Instant::now() >= deadline {
            return status;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn all_ids(storage_root: &Path) -> Vec<String> {
    let out = ociman(storage_root, &["ps", "-a", "-q"]);
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

/// `ociman container prune` removes only `Created`/`Stopped`
/// containers, leaves a genuinely `Running` one completely untouched,
/// and prints one line per removed id (no heading), matching real
/// `podman container prune`'s own `PrintContainerPruneResults
/// (responses, false)` exactly.
#[test]
fn container_prune_removes_created_and_stopped_but_not_running() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        storage_dir.path().join(".rootless-overlay-supported"),
        "false",
    )
    .unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/container-prune:latest",
        &busybox,
        &["sh", "true", "sleep"],
        ContainerConfig::default(),
    );

    // A `Created` (real, never-started) container.
    let create = ociman(
        storage_dir.path(),
        &["create", "ociman-test/container-prune:latest", "true"],
    );
    assert!(create.status.success(), "{create:?}");
    let created_id = String::from_utf8_lossy(&create.stdout).trim().to_string();
    assert_eq!(
        inspect_json(storage_dir.path(), &created_id)["status"],
        "created"
    );

    // A `Stopped` (real, already-exited) container. `ociman run`
    // (foreground) never prints the container's own id on success
    // (only the container's own output/exit code do, matching real
    // `podman run` exactly) — the new id is found by diffing `ps -a
    // -q` against what's already known, the same technique
    // `rm_all_removes_every_stopped_container` above already
    // established via a plain count.
    let run = ociman(
        storage_dir.path(),
        &["run", "ociman-test/container-prune:latest", "true"],
    );
    assert!(run.status.success(), "{run:?}");
    let stopped_id = all_ids(storage_dir.path())
        .into_iter()
        .find(|id| id != &created_id)
        .expect("a second container (the one just run) should now exist");
    assert_eq!(
        inspect_json(storage_dir.path(), &stopped_id)["status"],
        "stopped"
    );

    // A genuinely `Running` container — must survive `prune`
    // untouched.
    ociman_run_detached(
        storage_dir.path(),
        "ociman-test/container-prune:latest",
        &["sleep", "30"],
    );
    let running_id = all_ids(storage_dir.path())
        .into_iter()
        .find(|id| id != &created_id && id != &stopped_id)
        .expect("a third container (the detached one) should now exist");
    assert_eq!(
        wait_for_status(
            storage_dir.path(),
            &running_id,
            "running",
            Duration::from_secs(20)
        ),
        "running",
        "the third container should genuinely be running before prune is even attempted"
    );

    let prune = ociman(storage_dir.path(), &["container", "prune"]);
    assert!(
        prune.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&prune.stderr)
    );
    let mut pruned: Vec<String> = String::from_utf8_lossy(&prune.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    pruned.sort();
    let mut expected = vec![created_id.clone(), stopped_id.clone()];
    expected.sort();
    assert_eq!(pruned, expected, "{prune:?}");

    // Only the running container is left.
    let remaining = all_ids(storage_dir.path());
    assert_eq!(remaining, vec![running_id.clone()]);

    // A second `prune` with nothing left to remove prints nothing and
    // still succeeds (no "nothing to prune" false-error), matching
    // `ociman volume prune`'s own already-established empty-result
    // convention.
    let prune_again = ociman(storage_dir.path(), &["container", "prune"]);
    assert!(prune_again.status.success());
    assert!(
        String::from_utf8_lossy(&prune_again.stdout)
            .trim()
            .is_empty()
    );

    // Clean up the still-running container so the temp dir doesn't
    // leak a live process past this test.
    let _ = ociman(storage_dir.path(), &["kill", &running_id]);
}

/// `-f`/`--force` is accepted (real CLI compatibility with `podman
/// container prune --force`) but changes nothing: this project has no
/// interactive confirmation prompt to skip in the first place.
#[test]
fn container_prune_force_is_accepted_and_behaves_identically() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/container-prune-force:latest",
        &busybox,
        &["sh", "true"],
        ContainerConfig::default(),
    );
    let run = ociman(
        storage_dir.path(),
        &["run", "ociman-test/container-prune-force:latest", "true"],
    );
    assert!(run.status.success(), "{run:?}");
    let id = all_ids(storage_dir.path())
        .into_iter()
        .next()
        .expect("the just-run container should exist");

    let prune = ociman(storage_dir.path(), &["container", "prune", "--force"]);
    assert!(prune.status.success(), "{prune:?}");
    assert_eq!(String::from_utf8_lossy(&prune.stdout).trim(), id);
    assert!(all_ids(storage_dir.path()).is_empty());
}

/// `container prune --json` emits the same removed-id list as a plain
/// JSON array, matching `volume prune --json`'s own already-
/// established shape.
#[test]
fn container_prune_json_emits_an_array_of_removed_ids() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/container-prune-json:latest",
        &busybox,
        &["sh", "true"],
        ContainerConfig::default(),
    );
    let run = ociman(
        storage_dir.path(),
        &["run", "ociman-test/container-prune-json:latest", "true"],
    );
    assert!(run.status.success(), "{run:?}");
    let id = all_ids(storage_dir.path())
        .into_iter()
        .next()
        .expect("the just-run container should exist");

    let prune = ociman(storage_dir.path(), &["--json", "container", "prune"]);
    assert!(prune.status.success(), "{prune:?}");
    let json: serde_json::Value = serde_json::from_slice(&prune.stdout).unwrap();
    assert_eq!(json, serde_json::json!([id]));
}
