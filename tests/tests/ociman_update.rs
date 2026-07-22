//! `ociman update` integration tests: changing a running container's
//! real cgroup resource limits in place, matching real `podman
//! update` for the same subset of resource flags `ociman run` itself
//! already supports (see `docs/design/0171`). Same fully offline
//! seeded-image approach `ociman_kill.rs`/`ociman_stop.rs` established,
//! including the same `spawn()`+detached-stdio+poll concurrency
//! pattern for a container that needs to still be running while a
//! separate invocation acts on it.

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

/// The real cgroup v2 file this container's own memory limit lives in
/// (`cgroup.freeze`'s own sibling directory this project's own
/// `oci_runtime_core::cgroups` already resolves elsewhere) -- read
/// directly rather than through another layer of this project's own
/// code, so this test is checking the real, final kernel-visible
/// effect, not just that some internal function was called.
fn real_cgroup_dir_for(storage_root: &Path, id: &str) -> std::path::PathBuf {
    let containers = oci_runtime_core::StateStore::open(storage_root.join("containers")).unwrap();
    let state = containers.load(id).unwrap();
    let pid = state.pid.expect("running container must have a pid");
    oci_runtime_core::cgroups::cgroup_dir_for_running_pid(Path::new("/sys/fs/cgroup"), pid)
        .expect("resolving real cgroup for a running container")
}

#[test]
fn update_of_an_unknown_container_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let update = ociman(
        storage_dir.path(),
        &["update", "--memory", "64m", "never-existed"],
    );
    assert!(!update.status.success());
    assert!(
        String::from_utf8_lossy(&update.stderr).contains("does not exist"),
        "{}",
        String::from_utf8_lossy(&update.stderr)
    );
}

#[test]
fn update_with_no_resource_flags_at_all_is_a_clear_error() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/update-no-flags:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );
    let mut child = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/update-no-flags:latest",
        &["-d", "sh", "-c", "sleep 30"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20));

    let update = ociman(storage_dir.path(), &["update", &id]);
    assert!(!update.status.success());
    assert!(
        String::from_utf8_lossy(&update.stderr).contains("no resource flags"),
        "{}",
        String::from_utf8_lossy(&update.stderr)
    );

    ociman(storage_dir.path(), &["kill", &id]);
    child.wait().ok();
}

#[test]
fn update_of_an_already_stopped_container_is_a_clear_error() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/update-stopped:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "true".to_string(),
            ]),
            ..Default::default()
        },
    );
    let run = ociman(
        storage_dir.path(),
        &["run", "ociman-test/update-stopped:latest"],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let ps = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    let id = String::from_utf8_lossy(&ps.stdout).trim().to_string();
    assert!(!id.is_empty());

    let update = ociman(storage_dir.path(), &["update", "--memory", "64m", &id]);
    assert!(!update.status.success());
    assert!(
        String::from_utf8_lossy(&update.stderr).contains("not running"),
        "{}",
        String::from_utf8_lossy(&update.stderr)
    );
}

/// The real, convincing check: update a genuinely running container's
/// `--memory`/`--cpus`/`--pids-limit`, then read the real cgroup v2
/// accounting files back directly to confirm the kernel itself now
/// enforces the new limits -- not just that `ociman update` exited
/// `0`.
#[test]
fn update_changes_the_real_live_cgroup_limits_of_a_running_container() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/update-live:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );
    let mut child = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/update-live:latest",
        &["-d", "sh", "-c", "sleep 30"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20));

    let update = ociman(
        storage_dir.path(),
        &[
            "update",
            "--memory",
            "64m",
            "--cpus",
            "0.5",
            "--pids-limit",
            "42",
            &id,
        ],
    );
    assert!(
        update.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&update.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&update.stdout).trim(), id);

    let cgroup_dir = real_cgroup_dir_for(storage_dir.path(), &id);
    let memory_max = std::fs::read_to_string(cgroup_dir.join("memory.max")).unwrap();
    assert_eq!(memory_max.trim(), (64 * 1024 * 1024).to_string());

    let cpu_max = std::fs::read_to_string(cgroup_dir.join("cpu.max")).unwrap();
    // 0.5 CPUs -> a 50_000us quota over the fixed 100_000us period,
    // matching `resources_from_cli`'s own conversion.
    assert_eq!(cpu_max.trim(), "50000 100000");

    let pids_max = std::fs::read_to_string(cgroup_dir.join("pids.max")).unwrap();
    assert_eq!(pids_max.trim(), "42");

    ociman(storage_dir.path(), &["kill", &id]);
    child.wait().ok();
}
