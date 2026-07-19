//! `ociman stop` integration tests: a graceful-signal-then-`KILL`
//! policy on top of the same `oci_runtime_core::process::{kill,alive}`
//! primitives `rm --force` already uses (0021) — distinct from that
//! immediate `SIGKILL`, and distinct from `ocirun kill` (a single raw
//! signal with no wait/escalation policy at all, matching real
//! low-level runtimes' own minimal `kill` primitive).
//!
//! Same fully offline seeded-image approach `ociman_run.rs` established,
//! and the same `spawn()`+detached-stdio+poll concurrency pattern
//! `ociman_exec.rs`/`ociman_logs.rs` use for a container that needs to
//! still be running while a separate invocation acts on it.

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
fn stop_lets_a_signal_handling_container_exit_gracefully() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/stop-graceful:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/stop-graceful:latest",
        // A *single*, long-running foreground `sleep 30` here would be
        // a real footgun, not a flaky test: shells commonly defer
        // running a trap until the current foreground child actually
        // exits on its own (verified against a real kernel/busybox —
        // a `sleep 30` variant of this exact test took the entire
        // grace window rather than reacting to `TERM` promptly, even
        // though the trap itself was installed correctly). Looping
        // over short sleeps instead bounds how long a pending trap can
        // possibly be deferred to a fraction of a second, regardless.
        &[
            "/bin/sh",
            "-c",
            "trap 'exit 0' TERM; while true; do sleep 0.2; done",
        ],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(5));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(5)),
        "running"
    );

    // A generous grace window: what actually matters here is *whether*
    // the trap gets to run at all before any `KILL` escalation, not
    // exactly how many milliseconds that takes (real OS scheduling
    // jitter, especially under a loaded host, makes any assertion on
    // elapsed wall-clock time flaky by nature -- an earlier version of
    // this test asserted `stop` returned quickly and intermittently
    // failed under host load for exactly that reason; the *exit code*
    // check below is the deterministic, meaningful assertion: a `KILL`
    // escalation would produce 137, not the trap's own `exit 0`). 60s
    // (not the original 20s) after this test was *still* observed to
    // occasionally take the entire window and escalate to `KILL` on
    // this project's own shared dev host under heavy, unrelated
    // concurrent load (a separate session's own `cargo build --release
    // -C lto=fat -C codegen-units=1`, confirmed directly via `ps` at
    // the exact time of the failure) — the normal, uncontended case
    // still finishes in milliseconds regardless of how generous this
    // ceiling is, so raising it only helps, never slows down the
    // common case.
    let stop = ociman(storage_dir.path(), &["stop", "--time", "60", &id]);
    assert!(
        stop.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&stop.stderr)
    );

    run.wait().unwrap();
    let ps = ociman(storage_dir.path(), &["ps", "-a", "--json"]);
    let views: serde_json::Value = serde_json::from_slice(&ps.stdout).unwrap();
    let entry = views.as_array().unwrap().iter().find(|e| e["id"] == id);
    let entry = entry.expect("container should still be listed");
    assert_eq!(entry["status"], "stopped");
    assert_eq!(
        entry["exit_code"], 0,
        "a graceful exit(0) from the TERM trap, not a KILL exit code: {entry:?}"
    );

    ociman(storage_dir.path(), &["rm", &id]);
}

#[test]
fn stop_escalates_to_kill_when_the_container_ignores_the_signal() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/stop-escalate:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/stop-escalate:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(5));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(5)),
        "running"
    );

    // A plain `sleep 30` run as a pid-namespace's own init ignores an
    // unhandled-default-action `TERM` outright (0017's own finding) --
    // a real, deliberately short grace window here so this test
    // doesn't have to wait long to observe the escalation to `KILL`.
    let stop = ociman(storage_dir.path(), &["stop", "--time", "1", &id]);
    assert!(
        stop.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&stop.stderr)
    );

    run.wait().unwrap();
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "stopped", Duration::from_secs(5)),
        "stopped"
    );

    ociman(storage_dir.path(), &["rm", &id]);
}

#[test]
fn stop_is_a_noop_on_an_already_stopped_container() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/stop-already-stopped:latest",
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
        &["run", "ociman-test/stop-already-stopped:latest"],
    );
    assert!(run.status.success());
    let id = only_container_id(storage_dir.path(), Duration::from_secs(5));
    assert!(!id.is_empty());

    let stop = ociman(storage_dir.path(), &["stop", &id]);
    assert!(
        stop.status.success(),
        "stop on an already-stopped container should be a no-op, not an error: {}",
        String::from_utf8_lossy(&stop.stderr)
    );
}

#[test]
fn stop_of_a_nonexistent_container_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(storage_dir.path(), &["stop", "does-not-exist"]);
    assert!(!out.status.success());
}
