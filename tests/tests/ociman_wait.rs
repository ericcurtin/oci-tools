//! `ociman wait` integration tests: block until a container stops,
//! then print its exit code — matching real `docker wait`/`podman
//! wait` exactly (`~/git/podman/cmd/podman/containers/wait.go`: block,
//! then print a bare exit-code integer, nothing else). The exit code
//! itself is whatever `cmd_run`'s own foreground wait already
//! recorded (`ANNOTATION_EXIT_CODE`) — `wait` adds no new state of
//! its own.
//!
//! Same fully offline seeded-image approach `ociman_run.rs`
//! established, and the same `spawn()`+detached-stdio+poll concurrency
//! pattern `ociman_stop.rs`/`ociman_kill.rs` use for a container that
//! needs to still be running while a separate invocation acts on it.

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

fn wait_for_container_status(
    storage_root: &Path,
    id: &str,
    want: &str,
    timeout: Duration,
) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let out = ociman(storage_root, &["ps", "-a", "--json"]);
        if out.status.success()
            && let Ok(views) = serde_json::from_slice::<serde_json::Value>(&out.stdout)
            && let Some(entry) = views
                .as_array()
                .and_then(|a| a.iter().find(|e| e["id"] == id))
        {
            let status = entry["status"].as_str().unwrap_or_default().to_string();
            if status == want || Instant::now() >= deadline {
                return status;
            }
        } else if Instant::now() >= deadline {
            return String::new();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn wait_blocks_until_the_container_exits_then_prints_its_real_exit_code() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/wait-basic:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/wait-basic:latest",
        &["/bin/sh", "-c", "sleep 1; exit 7"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let started = Instant::now();
    let wait = ociman(storage_dir.path(), &["wait", &id]);
    let elapsed = started.elapsed();
    assert!(
        wait.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&wait.stderr)
    );
    assert!(
        elapsed >= Duration::from_millis(500),
        "wait should have genuinely blocked until the container exited: {elapsed:?}"
    );
    assert_eq!(
        String::from_utf8_lossy(&wait.stdout).trim(),
        "7",
        "wait should print the real exit code"
    );

    run.wait().unwrap();
    ociman(storage_dir.path(), &["rm", &id]);
}

#[test]
fn wait_on_an_already_stopped_container_returns_immediately() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/wait-already-stopped:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "exit 42".to_string(),
            ]),
            ..Default::default()
        },
    );

    let run = ociman(
        storage_dir.path(),
        &["run", "ociman-test/wait-already-stopped:latest"],
    );
    // `ociman run`'s own exit code mirrors the container's real exit
    // code (42 here), not a plain success/failure boolean -- matching
    // real `podman run`/`ocirun run`.
    assert_eq!(run.status.code(), Some(42));
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());

    let started = Instant::now();
    let wait = ociman(storage_dir.path(), &["wait", &id]);
    let elapsed = started.elapsed();
    assert!(
        wait.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&wait.stderr)
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "wait on an already-stopped container should return immediately: {elapsed:?}"
    );
    assert_eq!(String::from_utf8_lossy(&wait.stdout).trim(), "42");
}

#[test]
fn wait_on_a_nonexistent_container_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(storage_dir.path(), &["wait", "does-not-exist"]);
    assert!(!out.status.success());
}
