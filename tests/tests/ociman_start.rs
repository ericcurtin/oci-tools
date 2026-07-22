//! `ociman start`/`ociman restart` integration tests (0154): re-running
//! an already-`Stopped` container's own already-on-disk bundle exactly
//! as `run` originally left it (`start`), and `stop`-then-`start`
//! (`restart`).
//!
//! Same fully offline seeded-image approach `ociman_run.rs` established.
//!
//! A real, previously-hit race is specifically covered here (not just
//! hypothesized): `stop_container` (shared by `cmd_stop` and
//! `cmd_restart`) can observe a container as "stopped" purely because
//! its own recorded pid is no longer alive, even while its own
//! detached *keeper* process has not yet finished writing the final
//! `Stopped` state to disk. Proceeding to launch a brand new container
//! immediately in that case let the *old* keeper's own delayed
//! terminal write silently clobber the *new* one's fresh `Creating`/
//! `Running` state moments later — `restart_reruns_the_container_a_
//! third_time` below reproduced this quite reliably (well over half of
//! repeated runs) before the fix.

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

fn only_container_id(storage_root: &Path, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let out = ociman(storage_root, &["ps", "-a", "-q"]);
        let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !id.is_empty() || Instant::now() >= deadline {
            return id;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
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

/// A container whose own command appends one line to `/marker.txt`
/// each time it actually runs, then exits immediately — deliberately
/// fast (rather than long-running), both to exercise the exact
/// pid-dies-before-its-keeper-finalizes race the 0154 fix addresses,
/// and to make counting real executions via the marker file's own
/// line count a simple, unambiguous assertion.
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

fn marker_contents(storage_root: &Path, id: &str) -> String {
    let rootfs = inspect_json(storage_root, id)["rootfs"]
        .as_str()
        .expect("inspect --json should report rootfs")
        .to_string();
    std::fs::read_to_string(Path::new(&rootfs).join("marker.txt")).unwrap_or_default()
}

#[test]
fn start_reruns_an_already_stopped_container() {
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
    seed_marker_image(&store, "ociman-test/start-basic:latest", &busybox);

    let run = ociman(
        storage_dir.path(),
        &["run", "ociman-test/start-basic:latest"],
    );
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_status(storage_dir.path(), &id, "stopped", Duration::from_secs(20)),
        "stopped"
    );
    assert_eq!(marker_contents(storage_dir.path(), &id), "hi\n");

    let start = ociman(storage_dir.path(), &["start", &id]);
    assert!(
        start.status.success(),
        "{}",
        String::from_utf8_lossy(&start.stderr)
    );
    assert_eq!(
        wait_for_status(storage_dir.path(), &id, "stopped", Duration::from_secs(20)),
        "stopped"
    );
    assert_eq!(
        marker_contents(storage_dir.path(), &id),
        "hi\nhi\n",
        "start should have run the same container's own command a second time"
    );

    ociman(storage_dir.path(), &["rm", &id]);
}

#[test]
fn restart_reruns_an_already_stopped_container_a_third_time() {
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
    seed_marker_image(&store, "ociman-test/restart-basic:latest", &busybox);

    let run = ociman(
        storage_dir.path(),
        &["run", "ociman-test/restart-basic:latest"],
    );
    assert!(run.status.success());
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_status(storage_dir.path(), &id, "stopped", Duration::from_secs(20)),
        "stopped"
    );

    let start = ociman(storage_dir.path(), &["start", &id]);
    assert!(start.status.success());
    assert_eq!(
        wait_for_status(storage_dir.path(), &id, "stopped", Duration::from_secs(20)),
        "stopped"
    );
    assert_eq!(marker_contents(storage_dir.path(), &id), "hi\nhi\n");

    // `restart` on an already-stopped container: matches real
    // podman's own `restartWithTimeout` (stop only if actually
    // running, start regardless).
    let restart = ociman(storage_dir.path(), &["restart", &id]);
    assert!(
        restart.status.success(),
        "{}",
        String::from_utf8_lossy(&restart.stderr)
    );
    assert_eq!(
        wait_for_status(storage_dir.path(), &id, "stopped", Duration::from_secs(20)),
        "stopped"
    );
    assert_eq!(
        marker_contents(storage_dir.path(), &id),
        "hi\nhi\nhi\n",
        "restart should have run the container a third time"
    );

    ociman(storage_dir.path(), &["rm", &id]);
}

#[test]
fn restart_stops_a_running_container_before_starting_it_again() {
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
        "ociman-test/restart-running:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "-d",
            "ociman-test/restart-running:latest",
            "/bin/sh",
            "-c",
            "sleep 30",
        ],
    );
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let id = String::from_utf8_lossy(&run.stdout).trim().to_string();
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );
    let first_pid = inspect_json(storage_dir.path(), &id)["pid"]
        .as_i64()
        .expect("running container should report a real pid");

    let restart = ociman(storage_dir.path(), &["restart", "--time", "1", &id]);
    assert!(
        restart.status.success(),
        "{}",
        String::from_utf8_lossy(&restart.stderr)
    );
    assert_eq!(
        wait_for_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running",
        "restart should leave a freshly-restarted long-running container running"
    );
    let second_pid = inspect_json(storage_dir.path(), &id)["pid"]
        .as_i64()
        .expect("running container should report a real pid");
    assert_ne!(
        first_pid, second_pid,
        "restart should have replaced the container's own process with a new one"
    );

    ociman(storage_dir.path(), &["stop", "--time", "0", &id]);
    ociman(storage_dir.path(), &["rm", "-f", &id]);
}

#[test]
fn start_on_an_already_running_container_is_a_clear_error() {
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
        "ociman-test/start-already-running:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "-d",
            "ociman-test/start-already-running:latest",
            "/bin/sh",
            "-c",
            "sleep 30",
        ],
    );
    assert!(run.status.success());
    let id = String::from_utf8_lossy(&run.stdout).trim().to_string();
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let start = ociman(storage_dir.path(), &["start", &id]);
    assert!(!start.status.success());

    ociman(storage_dir.path(), &["stop", "--time", "0", &id]);
    ociman(storage_dir.path(), &["rm", "-f", &id]);
}

#[test]
fn start_of_a_nonexistent_container_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(storage_dir.path(), &["start", "does-not-exist"]);
    assert!(!out.status.success());
}

#[test]
fn restart_of_a_nonexistent_container_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(storage_dir.path(), &["restart", "does-not-exist"]);
    assert!(!out.status.success());
}
