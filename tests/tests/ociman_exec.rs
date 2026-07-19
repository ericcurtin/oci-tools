//! `ociman exec` integration tests: running an additional process
//! inside an already-running `ociman run` container, exercised end to
//! end with the same fully offline seeded-image approach `ociman_run.rs`
//! established (no registry access needed).
//!
//! Unlike `ociman run` itself (which blocks in the foreground until the
//! container exits), these tests need a container that's still
//! *running* while a separate `ociman exec` invocation acts on it — so
//! `run` is `spawn()`ed (not `.output()`ed) with its own stdio detached
//! (same reasoning `oci_tools_tests::ocirun_create` already documents:
//! a real pipe would never see EOF until the backgrounded process
//! itself exits) and polled via `ociman ps` until its status is
//! `running` before `exec` is attempted.

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

/// Start `ociman run <image> <container args>` in the background
/// (detached stdio — see this file's own doc comment), returning the
/// child handle so the caller can eventually reap it.
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

/// Poll `ociman ps -a --json`'s single-container status field until it
/// equals `want` or `timeout` elapses.
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
fn exec_joins_a_still_running_ociman_run_container() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/exec-basic:latest",
        &busybox,
        &["sh", "ps"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/exec-basic:latest",
        &["/bin/sh", "-c", "sleep 5"],
    );

    // Find the container's id via `ps -a` (only one exists).
    let id = {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let out = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
            let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !id.is_empty() || Instant::now() >= deadline {
                break id;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    assert!(!id.is_empty(), "expected a container id to appear in ps -a");
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(5)),
        "running",
        "container never reached 'running' before exec was attempted"
    );

    let exec = ociman(
        storage_dir.path(),
        &[
            "exec",
            &id,
            "/bin/sh",
            "-c",
            "echo exec-worked-in-ociman; ps aux",
        ],
    );
    assert!(
        exec.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&exec.stderr)
    );
    let stdout = String::from_utf8_lossy(&exec.stdout).into_owned();
    assert!(stdout.contains("exec-worked-in-ociman"), "got: {stdout:?}");
    assert!(
        stdout.contains("sleep 5"),
        "exec'd process should see the container's own init in `ps`: {stdout:?}"
    );

    // The container itself must still be running after `exec` returns.
    assert_eq!(
        wait_for_container_status(
            storage_dir.path(),
            &id,
            "running",
            Duration::from_millis(200)
        ),
        "running"
    );

    run.wait().unwrap();
    ociman(storage_dir.path(), &["rm", "--force", &id]);
}

#[test]
fn exec_refuses_a_container_that_has_already_stopped() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/exec-stopped:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "ociman-test/exec-stopped:latest",
            "/bin/sh",
            "-c",
            "true",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    let id = {
        let out = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    assert!(!id.is_empty());

    let exec = ociman(storage_dir.path(), &["exec", &id, "/bin/sh", "-c", "true"]);
    assert!(
        !exec.status.success(),
        "exec should refuse an already-stopped container"
    );
}
