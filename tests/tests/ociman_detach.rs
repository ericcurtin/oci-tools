//! `ociman run -d`/`--detach` integration tests: matching real `docker
//! run -d`/`podman run -d` exactly — the CLI invocation itself returns
//! immediately, printing the container's own id, while the container
//! keeps running in the background (see `docs/design/0098`). Same
//! fully offline seeded-image approach `ociman_run.rs` established.

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

fn container_status(storage_root: &Path, id: &str) -> Option<String> {
    let out = ociman(storage_root, &["ps", "-a", "--json"]);
    if !out.status.success() {
        return None;
    }
    let views: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    views
        .as_array()?
        .iter()
        .find(|e| e["id"] == id)
        .map(|e| e["status"].as_str().unwrap_or_default().to_string())
}

fn wait_for_status(storage_root: &Path, id: &str, want: &str, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = container_status(storage_root, id) {
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
fn run_detach_returns_immediately_and_the_container_keeps_running() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/detach-basic:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let started = Instant::now();
    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "-d",
            "--name",
            "detach-basic",
            "ociman-test/detach-basic:latest",
            "/bin/sh",
            "-c",
            "sleep 15",
        ],
    );
    let elapsed = started.elapsed();
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    // A generous ceiling: what actually matters here is *whether*
    // `run -d` detached at all rather than waiting for the container's
    // own command to finish, not exactly how many milliseconds that
    // takes -- real OS/namespace-setup scheduling jitter, especially
    // under a loaded or slow-emulated (TCG, no KVM) CI host, makes a
    // tight assertion on elapsed wall-clock time flaky by nature (see
    // `ociman_stop.rs`'s own identical fix/finding for its own
    // grace-period assertion). The container's own command sleeps a
    // full 15s specifically so that even a generous multi-second
    // ceiling here still fails loudly if `run -d` genuinely blocked
    // until the container exited instead of actually detaching; the
    // common, uncontended case still returns in well under a second
    // regardless of how generous this ceiling is, so raising it only
    // helps, never slows down the common case.
    assert!(
        elapsed < Duration::from_secs(10),
        "run -d should return almost immediately, not wait for the container to exit: {elapsed:?}"
    );
    let id = String::from_utf8_lossy(&run.stdout).trim().to_string();
    assert!(!id.is_empty(), "run -d should print the real container id");

    // The container is genuinely still running right after `run -d`
    // itself already returned -- not merely "will eventually start".
    assert_eq!(
        container_status(storage_dir.path(), &id).as_deref(),
        Some("running")
    );

    assert_eq!(
        wait_for_status(storage_dir.path(), &id, "stopped", Duration::from_secs(30)),
        "stopped"
    );

    let logs = ociman(storage_dir.path(), &["logs", &id]);
    assert!(logs.status.success());

    ociman(storage_dir.path(), &["rm", &id]);
}

#[test]
fn run_detach_rm_removes_the_container_after_it_exits() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/detach-rm:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "exit 0".to_string(),
            ]),
            ..Default::default()
        },
    );

    let run = ociman(
        storage_dir.path(),
        &["run", "-d", "--rm", "ociman-test/detach-rm:latest"],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let id = String::from_utf8_lossy(&run.stdout).trim().to_string();
    assert!(!id.is_empty());

    // Poll for the container record itself to disappear (`--rm`'s own
    // effect), rather than a status value -- once removed, `ps -a`
    // simply won't list it at all any more.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if container_status(storage_dir.path(), &id).is_none() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "detached --rm container was never removed after exiting"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn run_detach_with_a_setup_failure_fails_synchronously_without_detaching() {
    let storage_dir = tempfile::tempdir().unwrap();
    // No image ever pulled/seeded at all -- a real, immediate
    // resolution failure, the same kind of setup error that must
    // never silently fork off a background process anyway.
    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "-d",
            "ociman-test/this-image-was-never-seeded:latest",
        ],
    );
    assert!(
        !run.status.success(),
        "a setup failure must be reported synchronously, not silently detached"
    );
    // No container should have been left behind at all.
    let ps = ociman(storage_dir.path(), &["ps", "-a", "--json"]);
    let views: serde_json::Value = serde_json::from_slice(&ps.stdout).unwrap();
    assert_eq!(views.as_array().unwrap().len(), 0, "{views:?}");
}
