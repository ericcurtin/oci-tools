//! `ociman create` integration tests (0157): pull/extract an image's
//! container exactly like `run`, but never launch it, leaving it in a
//! real `created` state for a later `ociman start` to actually run
//! for the first time.
//!
//! Same fully offline seeded-image approach `ociman_run.rs` established.

use std::path::Path;
use std::process::Command;
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

fn inspect_json(storage_root: &Path, id: &str) -> serde_json::Value {
    let out = ociman(storage_root, &["inspect", id, "--json"]);
    assert!(
        out.status.success(),
        "ociman inspect failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
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

fn marker_contents(storage_root: &Path, id: &str) -> String {
    let rootfs = inspect_json(storage_root, id)["rootfs"]
        .as_str()
        .expect("inspect --json should report rootfs")
        .to_string();
    std::fs::read_to_string(Path::new(&rootfs).join("marker.txt")).unwrap_or_default()
}

fn seed_marker_image(store: &Store, reference: &str, busybox: &Path) {
    seed_image(
        store,
        reference,
        busybox,
        &["sh"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "echo hi >> /marker.txt; exit 0".to_string(),
            ]),
            ..Default::default()
        },
    );
}

#[test]
fn create_leaves_a_real_created_container_hidden_from_plain_ps_but_visible_with_ps_a() {
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
    seed_marker_image(&store, "ociman-test/create-basic:latest", &busybox);

    let create = ociman(
        storage_dir.path(),
        &["create", "ociman-test/create-basic:latest"],
    );
    assert!(
        create.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&create.stderr)
    );
    let id = String::from_utf8_lossy(&create.stdout).trim().to_string();
    assert!(!id.is_empty());

    // Real, checked-directly against a real `podman create` + plain
    // `podman ps`: a never-started container is hidden by default.
    let ps = ociman(storage_dir.path(), &["ps", "-q"]);
    assert!(ps.status.success());
    assert!(
        String::from_utf8_lossy(&ps.stdout).trim().is_empty(),
        "a merely-created container must not show up in plain `ps`"
    );

    let ps_all = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    assert!(ps_all.status.success());
    assert_eq!(String::from_utf8_lossy(&ps_all.stdout).trim(), id);

    assert_eq!(inspect_json(storage_dir.path(), &id)["status"], "created");
    // Never ran at all yet -- the marker file the image's own command
    // would append to must not exist.
    assert_eq!(marker_contents(storage_dir.path(), &id), "");

    ociman(storage_dir.path(), &["rm", &id]);
}

#[test]
fn start_on_a_created_container_runs_it_for_the_first_time() {
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
    seed_marker_image(&store, "ociman-test/create-start:latest", &busybox);

    let create = ociman(
        storage_dir.path(),
        &["create", "ociman-test/create-start:latest"],
    );
    assert!(create.status.success());
    let id = String::from_utf8_lossy(&create.stdout).trim().to_string();

    let start = ociman(storage_dir.path(), &["start", &id]);
    assert!(
        start.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&start.stderr)
    );
    assert_eq!(
        wait_for_status(storage_dir.path(), &id, "stopped", Duration::from_secs(20)),
        "stopped"
    );
    assert_eq!(
        marker_contents(storage_dir.path(), &id),
        "hi\n",
        "start should have run the created container's own command for the first time"
    );

    // A second `start` on the now-`stopped` container re-runs it again
    // -- the exact same code path already established for `run` +
    // `start` (0154), now also reachable via `create` + `start`.
    let start2 = ociman(storage_dir.path(), &["start", &id]);
    assert!(start2.status.success());
    assert_eq!(
        wait_for_status(storage_dir.path(), &id, "stopped", Duration::from_secs(20)),
        "stopped"
    );
    assert_eq!(marker_contents(storage_dir.path(), &id), "hi\nhi\n");

    ociman(storage_dir.path(), &["rm", &id]);
}

#[test]
fn create_with_name_is_resolvable_by_name() {
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
    seed_marker_image(&store, "ociman-test/create-name:latest", &busybox);

    let create = ociman(
        storage_dir.path(),
        &[
            "create",
            "--name",
            "my-created-container",
            "ociman-test/create-name:latest",
        ],
    );
    assert!(
        create.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&create.stderr)
    );
    let id = String::from_utf8_lossy(&create.stdout).trim().to_string();

    let start = ociman(storage_dir.path(), &["start", "my-created-container"]);
    assert!(
        start.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&start.stderr)
    );
    assert_eq!(
        wait_for_status(storage_dir.path(), &id, "stopped", Duration::from_secs(20)),
        "stopped"
    );

    ociman(storage_dir.path(), &["rm", "my-created-container"]);
}

/// Poll `ociman ps -a -q` until `id` is no longer listed at all —
/// distinct from [`wait_for_status`], which needs the container to
/// still exist (`ociman inspect` would itself fail on a genuinely
/// removed one).
fn wait_until_removed(storage_root: &Path, id: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        let out = ociman(storage_root, &["ps", "-a", "-q"]);
        let still_present = String::from_utf8_lossy(&out.stdout)
            .lines()
            .any(|line| line == id);
        if !still_present || Instant::now() >= deadline {
            return !still_present;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn create_rm_auto_removes_the_container_once_it_finally_exits() {
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
    seed_marker_image(&store, "ociman-test/create-rm:latest", &busybox);

    let create = ociman(
        storage_dir.path(),
        &["create", "--rm", "ociman-test/create-rm:latest"],
    );
    assert!(
        create.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&create.stderr)
    );
    let id = String::from_utf8_lossy(&create.stdout).trim().to_string();
    assert_eq!(inspect_json(storage_dir.path(), &id)["status"], "created");

    let start = ociman(storage_dir.path(), &["start", &id]);
    assert!(
        start.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&start.stderr)
    );

    // `--rm` on `create` (0158) is only honored once *this* run
    // finally exits -- matching real `podman create --rm` exactly.
    assert!(
        wait_until_removed(storage_dir.path(), &id, Duration::from_secs(20)),
        "a container created with --rm should auto-remove once its own first real run exits"
    );
}

#[test]
fn create_of_a_nonexistent_image_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let create = ociman(
        storage_dir.path(),
        &["create", "does-not-exist/at-all:latest"],
    );
    assert!(!create.status.success());
}

/// `ociman create` shares `ociman run`'s own image-resolution logic
/// (`prepare_container`) entirely, so it can target an image by its
/// own real or short ID too, with no separate fix needed -- closing
/// the same gap `tests/tests/ociman_run.rs`'s own identical test
/// closes for `run` (0179/0180/0181 each separately named this
/// inconsistency).
#[test]
fn create_by_short_image_id_works() {
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
    seed_marker_image(&store, "ociman-test/create-by-id:latest", &busybox);
    let record = store
        .resolve_image("docker.io/ociman-test/create-by-id:latest")
        .unwrap()
        .unwrap();
    let short_id = &record.manifest_digest.hex()[..12];

    let create = ociman(storage_dir.path(), &["create", short_id]);
    assert!(
        create.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&create.stderr)
    );
    let id = String::from_utf8_lossy(&create.stdout).trim().to_string();
    assert_eq!(inspect_json(storage_dir.path(), &id)["status"], "created");

    let start = ociman(storage_dir.path(), &["start", &id]);
    assert!(
        start.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&start.stderr)
    );
}
