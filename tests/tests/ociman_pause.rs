//! `ociman pause`/`ociman unpause` integration tests: real cgroup v2
//! freezer support (see `docs/design/0143`) against a real, running,
//! systemd-cgroup-driver-managed container — `ociman run` always
//! attempts the systemd cgroup driver itself (no `systemd-run --user
//! --scope` carrier needed, matching `ociman_top.rs`'s own identical
//! reasoning), so this only needs a reachable `systemd --user`
//! session to skip cleanly where unavailable.

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

/// The `status` field from `ociman inspect <id> --json`, asserting
/// the command itself succeeded.
fn inspect_status(storage_root: &Path, id: &str) -> String {
    let out = ociman(storage_root, &["inspect", id, "--json"]);
    assert!(
        out.status.success(),
        "ociman inspect failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    json["status"].as_str().unwrap().to_string()
}

/// Same real, reachable-`systemd --user`-session probe
/// `ociman_top.rs`'s own tests use.
fn systemd_user_session_available() -> bool {
    Command::new("systemctl")
        .args(["--user", "is-system-running"])
        .output()
        .is_ok_and(|out| !out.stdout.is_empty())
}

/// The real pid `ociman top`'s own table shows for the container's
/// actual init process (the second, higher-numbered pid — the first
/// is always this test's own `ociman run` process itself).
fn container_init_pid(storage_root: &Path, id: &str) -> i32 {
    let top = ociman(storage_root, &["top", id]);
    assert!(top.status.success());
    let stdout = String::from_utf8_lossy(&top.stdout);
    let last_line = stdout.lines().next_back().expect("at least one pid line");
    last_line
        .split_whitespace()
        .nth(1)
        .expect("a PID column")
        .parse()
        .expect("a real numeric pid")
}

/// The real cgroup directory a running container's own init process is
/// actually in right now, read directly from `/proc/<pid>/cgroup` —
/// the exact same real resolution `ociman`'s own `resolve_running_
/// container_cgroup` uses internally, reused here so this test can
/// observe the real `cpu.stat`/`cgroup.freeze` files independently of
/// `ociman`'s own implementation.
fn real_cgroup_dir(pid: i32) -> std::path::PathBuf {
    let contents = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap();
    let relative = contents
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .expect("a real cgroup v2 (\"0::\") entry");
    Path::new("/sys/fs/cgroup").join(relative.trim_start_matches('/'))
}

fn cpu_usage_usec(cgroup_dir: &Path) -> u64 {
    std::fs::read_to_string(cgroup_dir.join("cpu.stat"))
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("usage_usec "))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

/// `ociman pause`/`ociman unpause` against a real, running, CPU-
/// burning container: pausing must make the cgroup's own real
/// `cpu.stat`'s `usage_usec` counter stop moving *entirely* for a
/// real, measured wall-clock interval, and unpausing must make it
/// start moving again — the actual, real kernel-level effect these
/// commands exist for, not just that the CLI calls themselves exit
/// successfully. Same real end-to-end verification technique
/// `ocirun_lifecycle.rs`'s own `pause_freezes_and_resume_thaws_a_
/// real_running_containers_own_cpu_usage` test already established.
#[test]
fn pause_freezes_and_unpause_thaws_a_real_running_containers_own_cpu_usage() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    if !systemd_user_session_available() {
        eprintln!("skipping: no reachable `systemd --user` session");
        return;
    }
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/pause-basic:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/pause-basic:latest",
        &["/bin/sh", "-c", "i=0; while true; do i=$((i+1)); done"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let pid = container_init_pid(storage_dir.path(), &id);
    let cgroup_dir = real_cgroup_dir(pid);

    // Let it genuinely burn some real CPU before pausing.
    std::thread::sleep(Duration::from_millis(300));

    let pause = ociman(storage_dir.path(), &["pause", &id]);
    assert!(
        pause.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&pause.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&pause.stdout).trim(),
        id,
        "pause should print the container id back, matching real podman"
    );
    assert_eq!(
        std::fs::read_to_string(cgroup_dir.join("cgroup.freeze"))
            .unwrap()
            .trim(),
        "1"
    );
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "paused", Duration::from_secs(5)),
        "paused",
        "ociman ps should report the real, computed \"paused\" status once frozen"
    );
    assert_eq!(
        inspect_status(storage_dir.path(), &id),
        "paused",
        "ociman inspect should also report the real, computed \"paused\" status once frozen"
    );

    let usage_just_after_pause = cpu_usage_usec(&cgroup_dir);
    std::thread::sleep(Duration::from_millis(500));
    let usage_after_waiting_while_frozen = cpu_usage_usec(&cgroup_dir);
    assert_eq!(
        usage_just_after_pause, usage_after_waiting_while_frozen,
        "a real frozen container must not consume any more CPU at all while paused"
    );

    let unpause = ociman(storage_dir.path(), &["unpause", &id]);
    assert!(
        unpause.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&unpause.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(cgroup_dir.join("cgroup.freeze"))
            .unwrap()
            .trim(),
        "0"
    );
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(5)),
        "running",
        "ociman ps should report \"running\" again once genuinely thawed"
    );
    assert_eq!(
        inspect_status(storage_dir.path(), &id),
        "running",
        "ociman inspect should also report \"running\" again once genuinely thawed"
    );

    std::thread::sleep(Duration::from_millis(300));
    let usage_after_unpause = cpu_usage_usec(&cgroup_dir);
    assert!(
        usage_after_unpause > usage_after_waiting_while_frozen,
        "a real unpaused container must start consuming CPU again \
         (frozen: {usage_after_waiting_while_frozen}, after unpause: {usage_after_unpause})"
    );

    let kill = ociman(storage_dir.path(), &["kill", &id]);
    assert!(kill.status.success());
    run.wait().unwrap();
    ociman(storage_dir.path(), &["rm", &id]);
}

/// `pause`/`unpause` against a container that has already stopped is
/// a clear, real error, not a silent no-op.
#[test]
fn pause_and_unpause_on_a_stopped_container_are_clear_errors() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/pause-stopped:latest",
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
        &["run", "ociman-test/pause-stopped:latest"],
    );
    assert!(run.status.success());
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());

    let pause = ociman(storage_dir.path(), &["pause", &id]);
    assert!(!pause.status.success());

    let unpause = ociman(storage_dir.path(), &["unpause", &id]);
    assert!(!unpause.status.success());
}

#[test]
fn pause_and_unpause_on_an_unknown_container_are_clear_errors() {
    let storage_dir = tempfile::tempdir().unwrap();
    let pause = ociman(storage_dir.path(), &["pause", "does-not-exist"]);
    assert!(!pause.status.success());
    let unpause = ociman(storage_dir.path(), &["unpause", "does-not-exist"]);
    assert!(!unpause.status.success());
}
