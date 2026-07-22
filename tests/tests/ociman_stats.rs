//! `ociman stats --no-stream` integration tests: a real, one-shot
//! cgroup-v2-accounting sample for a running container (see
//! `docs/design/0145`) — same "no `systemd-run --user --scope` carrier
//! needed" reasoning `ociman_top.rs`/`ociman_pause.rs` already
//! establish (`ociman run` always attempts the systemd cgroup driver
//! itself), so this only needs a reachable `systemd --user` session to
//! skip cleanly where unavailable.

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
/// `ociman_top.rs`/`ociman_pause.rs`'s own tests use.
fn systemd_user_session_available() -> bool {
    Command::new("systemctl")
        .args(["--user", "is-system-running"])
        .output()
        .is_ok_and(|out| !out.stdout.is_empty())
}

/// `ociman stats --no-stream --json` against a real, running,
/// genuinely CPU-burning container: the real cgroup's own accounting
/// files must report a substantial, non-zero CPU percentage (it's
/// been consuming a full core continuously since it started) and a
/// real, non-zero memory usage — not just a successful exit code.
#[test]
fn stats_no_stream_reports_real_cpu_and_memory_usage_for_a_running_container() {
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
        "ociman-test/stats-basic:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/stats-basic:latest",
        &["/bin/sh", "-c", "i=0; while true; do i=$((i+1)); done"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    // Let it genuinely burn a full core for a real, measured interval
    // before sampling -- the very first sample's own CPU % is an
    // average over the container's *whole* life so far, back to its
    // own recorded `created` timestamp (see `cmd_stats`'s own doc
    // comment), which includes real, essentially-fixed setup time
    // (image/rootfs/cgroup/systemd-scope setup) before the container's
    // own process is even running yet -- a full 3 real seconds of
    // continuous 100%-core burn is enough for that fixed overhead to
    // stop dominating the ratio (confirmed empirically: a mere 500ms
    // burn measured as low as ~33%, well under the assertion below,
    // purely from setup overhead diluting it -- not a real bug).
    std::thread::sleep(Duration::from_millis(3000));

    let stats = ociman(storage_dir.path(), &["stats", &id, "--no-stream", "--json"]);
    assert!(
        stats.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&stats.stderr)
    );
    let view: serde_json::Value = serde_json::from_slice(&stats.stdout).unwrap();
    assert_eq!(view["id"], id);

    let cpu_percent = view["cpu_percent"].as_f64().unwrap();
    assert!(
        cpu_percent > 50.0,
        "a container burning a full core continuously since it started should show a \
         substantial CPU %, got {cpu_percent}"
    );

    let mem_usage = view["mem_usage"].as_u64().unwrap();
    assert!(
        mem_usage > 0,
        "a running container should use some real memory"
    );

    let mem_limit = view["mem_limit"].as_u64().unwrap();
    assert!(
        mem_limit > mem_usage,
        "with no --memory limit set, the (physical-RAM-clamped) limit should be far larger \
         than actual usage"
    );

    let mem_percent = view["mem_percent"].as_f64().unwrap();
    assert!((0.0..100.0).contains(&mem_percent));

    let pids = view["pids"].as_u64().unwrap();
    assert!(
        pids >= 1,
        "at least the container's own init process should be counted"
    );

    let kill = ociman(storage_dir.path(), &["kill", &id]);
    assert!(kill.status.success());
    run.wait().unwrap();
    ociman(storage_dir.path(), &["rm", &id]);
}

/// The real, non-JSON table output at least contains the expected
/// header columns and the container's own id.
#[test]
fn stats_no_stream_table_output_has_the_real_expected_columns() {
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
        "ociman-test/stats-table:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/stats-table:latest",
        &["/bin/sh", "-c", "while true; do sleep 1; done"],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let stats = ociman(storage_dir.path(), &["stats", &id, "--no-stream"]);
    assert!(
        stats.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&stats.stderr)
    );
    let stdout = String::from_utf8_lossy(&stats.stdout);
    assert!(stdout.contains("CPU %"));
    assert!(stdout.contains("MEM USAGE / LIMIT"));
    assert!(stdout.contains("MEM %"));
    assert!(stdout.contains("PIDS"));
    assert!(stdout.contains(&id));

    let kill = ociman(storage_dir.path(), &["kill", &id]);
    assert!(kill.status.success());
    run.wait().unwrap();
    ociman(storage_dir.path(), &["rm", &id]);
}

/// Bare `ociman stats <id>` (no `--no-stream`) is a clear, loud error
/// -- continuous streaming isn't implemented yet, see `cmd_stats`'s
/// own doc comment -- never a silent hang or a silently different
/// one-shot behavior.
#[test]
fn stats_without_no_stream_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(storage_dir.path(), &["stats", "does-not-exist"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("no-stream"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `stats` against a container that has already stopped is a clear,
/// real error, not stale/zeroed-out data.
#[test]
fn stats_on_a_stopped_container_is_a_clear_error() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/stats-stopped:latest",
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
        &["run", "ociman-test/stats-stopped:latest"],
    );
    assert!(run.status.success());
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());

    let stats = ociman(storage_dir.path(), &["stats", &id, "--no-stream"]);
    assert!(!stats.status.success());
}

#[test]
fn stats_on_an_unknown_container_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let stats = ociman(
        storage_dir.path(),
        &["stats", "does-not-exist", "--no-stream"],
    );
    assert!(!stats.status.success());
}
