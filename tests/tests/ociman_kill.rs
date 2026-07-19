//! `ociman kill` integration tests: a single, immediate signal send
//! with no wait/escalation policy at all — distinct from `ociman stop`
//! (see `ociman_stop.rs`'s own doc comment), matching real `docker
//! kill`/`podman kill` exactly (default signal `KILL`, one
//! `Kill(sig)` call, no waiting — checked directly against
//! `~/git/podman/cmd/podman/containers/kill.go`).
//!
//! Same fully offline seeded-image approach `ociman_run.rs`
//! established, and the same `spawn()`+detached-stdio+poll concurrency
//! pattern `ociman_stop.rs`/`ociman_exec.rs` use for a container that
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
fn kill_sends_a_real_sigkill_by_default_and_stops_the_container_immediately() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/kill-default:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/kill-default:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    // No `--signal` given at all -- real `docker`/`podman kill`'s own
    // default is `KILL`, not `TERM` (unlike `stop`'s own default).
    let kill = ociman(storage_dir.path(), &["kill", &id]);
    assert!(
        kill.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&kill.stderr)
    );

    run.wait().unwrap();
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "stopped", Duration::from_secs(20)),
        "stopped"
    );

    ociman(storage_dir.path(), &["rm", &id]);
}

/// A `sleep 30` run as a pid-namespace's own init ignores an
/// unhandled-default-action `TERM` outright (the same real,
/// already-established kernel finding `docs/design/0017` and
/// `ociman_stop.rs`'s own escalation test rely on) — `kill --signal
/// TERM`, unlike `stop`, never escalates at all, so the container
/// should genuinely still be running afterward. This is the expected,
/// correct behavior for a single-signal-send primitive, not a bug.
#[test]
fn kill_with_a_custom_signal_sends_exactly_that_signal_and_never_escalates() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/kill-term:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/kill-term:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let kill = ociman(storage_dir.path(), &["kill", "--signal", "TERM", &id]);
    assert!(
        kill.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&kill.stderr)
    );

    // Give the (never-escalating) `TERM` every chance to have taken
    // effect if it somehow were going to -- it shouldn't, and the
    // container should still be running.
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        wait_for_container_status(
            storage_dir.path(),
            &id,
            "running",
            Duration::from_millis(200)
        ),
        "running",
        "an unhandled TERM should be silently ignored by the pid-namespace init, and `kill` \
         itself never escalates"
    );

    // Real `KILL` cannot be ignored -- clean the container up for
    // real.
    let real_kill = ociman(storage_dir.path(), &["kill", &id]);
    assert!(real_kill.status.success());
    run.wait().unwrap();
    wait_for_container_status(storage_dir.path(), &id, "stopped", Duration::from_secs(20));
    ociman(storage_dir.path(), &["rm", &id]);
}

#[test]
fn kill_on_an_already_stopped_container_is_a_real_error() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/kill-already-stopped:latest",
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
        &["run", "ociman-test/kill-already-stopped:latest"],
    );
    assert!(run.status.success());
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());

    let kill = ociman(storage_dir.path(), &["kill", &id]);
    assert!(
        !kill.status.success(),
        "kill on an already-stopped container should be a real error, unlike `stop`'s own \
         no-op, matching real podman's own ErrCtrStateInvalid"
    );
}

#[test]
fn kill_of_a_nonexistent_container_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(storage_dir.path(), &["kill", "does-not-exist"]);
    assert!(!out.status.success());
}
