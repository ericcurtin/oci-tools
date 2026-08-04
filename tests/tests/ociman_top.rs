//! `ociman top` integration tests: real docker/podman's own `ps(1)`
//! passthrough — every real pid in a container's own (real, current,
//! systemd-cgroup-driver) cgroup, filtered into the real host `ps`
//! binary's own table output (see `docs/design/0095`; the shared
//! filtering logic itself, `oci_runtime_core::cgroups::
//! print_ps_table`, is already covered by `ocirun_ps.rs`'s own tests).
//! `ociman run` always attempts the systemd cgroup driver itself (no
//! `systemd-run --user --scope` carrier needed, unlike `ocirun`'s own
//! raw-cgroupfs-driver tests — see `ociman_run.rs`'s own cgroup tests
//! for the same reasoning), so this only needs a reachable
//! `systemd --user` session to skip cleanly where unavailable.

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

/// Same real, reachable-`systemd --user`-session probe
/// `ociman_run.rs`'s own cgroup tests use.
fn systemd_user_session_available() -> bool {
    Command::new("systemctl")
        .args(["--user", "is-system-running"])
        .output()
        .is_ok_and(|out| !out.stdout.is_empty())
}

#[test]
fn top_shows_the_containers_own_real_command() {
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
        "ociman-test/top-basic:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/top-basic:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let top = ociman(storage_dir.path(), &["top", &id]);
    assert!(
        top.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&top.stderr)
    );
    let stdout = String::from_utf8_lossy(&top.stdout);
    let header = stdout.lines().next().expect("a header line");
    assert!(header.contains("PID"), "{header:?}");
    assert!(
        stdout.contains("sleep 30"),
        "expected the container's own real command: {stdout:?}"
    );

    let kill = ociman(storage_dir.path(), &["kill", &id]);
    assert!(kill.status.success());
    run.wait().unwrap();
    ociman(storage_dir.path(), &["rm", &id]);
}

#[test]
fn top_passes_extra_arguments_straight_through_to_the_real_ps_binary() {
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
        "ociman-test/top-aux:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/top-aux:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    // Real `ps -ef`'s own header uses `UID`; real `ps aux`'s own uses
    // `USER` -- a real, observable difference proving the extra
    // argument genuinely reached the real host `ps` binary.
    let top = ociman(storage_dir.path(), &["top", &id, "aux"]);
    assert!(top.status.success());
    let stdout = String::from_utf8_lossy(&top.stdout);
    let header = stdout.lines().next().expect("a header line");
    assert!(header.contains("USER"), "{header:?}");

    let kill = ociman(storage_dir.path(), &["kill", &id]);
    assert!(kill.status.success());
    run.wait().unwrap();
    ociman(storage_dir.path(), &["rm", &id]);
}

#[test]
fn top_on_a_stopped_container_is_a_real_error() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/top-stopped:latest",
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
        &["run", "ociman-test/top-stopped:latest"],
    );
    assert!(run.status.success());
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());

    let top = ociman(storage_dir.path(), &["top", &id]);
    assert!(
        !top.status.success(),
        "top on a stopped container should be a real error"
    );
}

#[test]
fn top_on_an_unknown_container_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(storage_dir.path(), &["top", "does-not-exist"]);
    assert!(!out.status.success());
}

/// `ociman top --latest`/`-l` (matching real `podman top --latest`
/// exactly) shows the single, real most-recently-*created*
/// container's own processes -- an earlier container's own, genuinely
/// different command must never appear.
#[test]
fn top_latest_shows_only_the_most_recently_created_containers_own_processes() {
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
        "ociman-test/top-latest:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut older = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/top-latest:latest",
        &["/bin/sh", "-c", "sleep 31"],
    );
    let older_id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!older_id.is_empty());
    assert_eq!(
        wait_for_container_status(
            storage_dir.path(),
            &older_id,
            "running",
            Duration::from_secs(20)
        ),
        "running"
    );

    // A real, distinguishable creation-time gap.
    std::thread::sleep(Duration::from_secs(2));

    let mut newer = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/top-latest:latest",
        &["/bin/sh", "-c", "sleep 32"],
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    let newer_id = loop {
        let out = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
        let ids: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect();
        if let Some(id) = ids.iter().find(|id| *id != &older_id) {
            break id.clone();
        }
        assert!(Instant::now() < deadline, "second container never appeared");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(
        wait_for_container_status(
            storage_dir.path(),
            &newer_id,
            "running",
            Duration::from_secs(20)
        ),
        "running"
    );

    let top = ociman(storage_dir.path(), &["top", "--latest"]);
    assert!(
        top.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&top.stderr)
    );
    let stdout = String::from_utf8_lossy(&top.stdout);
    assert!(
        stdout.contains("sleep 32"),
        "--latest must show the most recently created container's own processes: {stdout:?}"
    );
    assert!(
        !stdout.contains("sleep 31"),
        "--latest must never show an earlier container's own processes: {stdout:?}"
    );

    ociman(storage_dir.path(), &["kill", &older_id]);
    ociman(storage_dir.path(), &["kill", &newer_id]);
    older.wait().ok();
    newer.wait().ok();
    ociman(storage_dir.path(), &["rm", "-a", "-f"]);
}

/// Real podman's own `top.go` has no explicit mutual-exclusivity
/// check between `--latest` and further positional arguments at all
/// (checked directly: `Args: cobra.ArbitraryArgs`) -- every element
/// simply becomes a `ps` descriptor/argument when `--latest` is
/// given, exactly like it would with an explicit container reference
/// consumed first instead.
#[test]
fn top_latest_with_extra_args_passes_them_to_ps() {
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
        "ociman-test/top-latest-args:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/top-latest-args:latest",
        &["/bin/sh", "-c", "sleep 30"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let top = ociman(storage_dir.path(), &["top", "--latest", "aux"]);
    assert!(
        top.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&top.stderr)
    );
    let stdout = String::from_utf8_lossy(&top.stdout);
    let header = stdout.lines().next().expect("a header line");
    assert!(header.contains("USER"), "{header:?}");

    ociman(storage_dir.path(), &["kill", &id]);
    run.wait().ok();
}

/// Neither `--latest` nor an explicit container at all is a real,
/// immediate error, matching real podman's own exact wording.
#[test]
fn top_with_no_container_and_no_latest_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(storage_dir.path(), &["top"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("you must provide the name or id of a running container"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `top --latest` on a genuinely empty store is a real, clear error,
/// matching real `podman top --latest`'s own `ErrNoSuchCtr`.
#[test]
fn top_latest_on_an_empty_store_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    let out = ociman(storage_dir.path(), &["top", "--latest"]);
    assert!(!out.status.success());
}
