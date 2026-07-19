//! `ociman logs` integration tests, exercised entirely offline: the
//! same fully seeded-image approach `ociman_run.rs`/`ociman_exec.rs`
//! established.
//!
//! `ociman run` captures a container's combined stdout/stderr to a
//! `container.log` file inside its own per-container directory as it
//! runs (see `docs/design/0025`), independently of also still echoing
//! it live to the terminal — these tests check both that a *finished*
//! container's full output can be read back after the fact, and that a
//! *still-running* one's output-so-far is already visible before it
//! exits (the same concurrent, `spawn()`-based approach
//! `ociman_exec.rs` uses, for the same reason: a real pipe here would
//! hang this test process's own `.output()` call until the backgrounded
//! container itself exits).

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

fn ociman_run_detached(
    storage_root: &Path,
    image: &str,
    container_args: &[&str],
) -> std::process::Child {
    Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
        .env_remove("OCI_TOOLS_LOG")
        .args(["run", image])
        .args(container_args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn ociman run")
}

/// A generous timeout: `ociman run` now attempts a real systemd cgroup
/// driver D-Bus round trip per container (`docs/design/0034`), which
/// can occasionally take noticeably longer under heavy *concurrent*
/// test-suite load — the ordinary case still resolves in milliseconds.
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

#[test]
fn logs_shows_a_finished_containers_combined_output() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/logs-finished:latest",
        &busybox,
        &["sh", "echo"],
        ContainerConfig::default(),
    );

    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "ociman-test/logs-finished:latest",
            "/bin/sh",
            "-c",
            "echo line-from-stdout; echo line-from-stderr 1>&2",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());

    let logs = ociman(storage_dir.path(), &["logs", &id]);
    assert!(
        logs.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&logs.stderr)
    );
    let stdout = String::from_utf8_lossy(&logs.stdout).into_owned();
    assert!(stdout.contains("line-from-stdout"), "got: {stdout:?}");
    assert!(stdout.contains("line-from-stderr"), "got: {stdout:?}");
}

#[test]
fn logs_shows_output_so_far_from_a_still_running_container() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/logs-running:latest",
        &busybox,
        &["sh", "sleep", "echo"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/logs-running:latest",
        &[
            "/bin/sh",
            "-c",
            "echo before-sleep; sleep 5; echo after-sleep",
        ],
    );

    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());

    // Poll `logs` until the pre-sleep line shows up (the tee thread
    // writing it is racing this test, not synchronized with it).
    let deadline = Instant::now() + Duration::from_secs(20);
    let stdout = loop {
        let logs = ociman(storage_dir.path(), &["logs", &id]);
        let stdout = String::from_utf8_lossy(&logs.stdout).into_owned();
        if stdout.contains("before-sleep") || Instant::now() >= deadline {
            break stdout;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(
        stdout.contains("before-sleep"),
        "expected the pre-sleep line to already be logged: {stdout:?}"
    );
    assert!(
        !stdout.contains("after-sleep"),
        "the post-sleep line shouldn't exist yet while the container is still sleeping: {stdout:?}"
    );

    run.wait().unwrap();
    let logs = ociman(storage_dir.path(), &["logs", &id]);
    let stdout = String::from_utf8_lossy(&logs.stdout).into_owned();
    assert!(stdout.contains("before-sleep"), "got: {stdout:?}");
    assert!(stdout.contains("after-sleep"), "got: {stdout:?}");

    ociman(storage_dir.path(), &["rm", "--force", &id]);
}

#[test]
fn logs_rejects_an_unknown_container_id() {
    let storage_dir = tempfile::tempdir().unwrap();
    let _store = Store::open(storage_dir.path()).unwrap();

    let logs = ociman(storage_dir.path(), &["logs", "no-such-container"]);
    assert!(
        !logs.status.success(),
        "logs should refuse an unknown container id"
    );
}
