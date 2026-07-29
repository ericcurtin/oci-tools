//! `ocirun events --stats` integration tests (`docs/design/0260`):
//! matches real `runc events --stats`'s own one-shot mode exactly —
//! see `Command::Events`'s own doc comment in `bin/ocirun/src/main.rs`
//! for exactly which fields this deliberately narrower report covers
//! and why. Needs a real, delegated `systemd --user` cgroup subtree to
//! actually exercise (same reasoning/setup `ocirun_update.rs`'s own
//! tests already established) — skips cleanly where unavailable.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use oci_tools_tests::{
    bin_path, busybox_path, ocirun, ocirun_create, wait_for_status, write_bundle,
};

/// Same real, reachable-`systemd --user`-session probe
/// `ocirun_update.rs`'s own tests use.
fn systemd_user_scope_available() -> bool {
    Command::new("systemd-run")
        .args(["--user", "--scope", "--", "true"])
        .output()
        .is_ok_and(|out| out.status.success())
}

/// The exact same real-cgroup fixture `ocirun_update.rs`'s own
/// `create_and_start_with_real_cgroup` establishes, kept as its own
/// small, deliberate duplicate here (this project's own convention:
/// small test-fixture duplication beats a new cross-test-file
/// dependency for four lines of setup).
fn create_and_start_with_real_cgroup(
    id: &str,
) -> Option<(tempfile::TempDir, tempfile::TempDir, std::path::PathBuf)> {
    if !systemd_user_scope_available() {
        eprintln!(
            "skipping: no reachable `systemd --user` session (systemd-run --user --scope failed)"
        );
        return None;
    }
    let busybox = busybox_path()?;
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "sleep 30"]);

    let config_path = bundle_dir.path().join("config.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    let uid = rustix::process::getuid().as_raw();
    let target = format!(
        "/user.slice/user-{uid}.slice/user@{uid}.service/app.slice/ocirun-events-test-{id}-{}",
        std::process::id()
    );
    config["linux"]["cgroupsPath"] = serde_json::json!(target);
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
    let cgroup_dir = Path::new("/sys/fs/cgroup").join(target.trim_start_matches('/'));

    let carrier_unit = format!(
        "ocirun-events-test-carrier-{id}-{}.scope",
        std::process::id()
    );
    let create = Command::new("systemd-run")
        .args([
            "--user",
            "--scope",
            "--slice=app.slice",
            &format!("--unit={carrier_unit}"),
            "--",
        ])
        .arg(bin_path("ocirun"))
        .args(["--root"])
        .arg(root_dir.path())
        .args(["create", id, "--bundle"])
        .arg(bundle_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .expect("failed to spawn systemd-run");
    assert!(
        create.status.success(),
        "create failed: {}",
        String::from_utf8_lossy(&create.stderr)
    );

    let start = ocirun(root_dir.path(), &["start", id]);
    assert!(
        start.status.success(),
        "start failed: {}",
        String::from_utf8_lossy(&start.stderr)
    );

    Some((bundle_dir, root_dir, cgroup_dir))
}

fn cleanup(root_dir: &Path, id: &str, cgroup_dir: &Path) {
    let kill = ocirun(root_dir, &["kill", id, "KILL"]);
    assert!(kill.status.success());
    wait_for_status(root_dir, id, "stopped", Duration::from_secs(5));
    let delete = ocirun(root_dir, &["delete", id]);
    assert!(delete.status.success());
    assert!(!cgroup_dir.exists());
}

#[test]
fn events_stats_reports_real_cgroup_numbers_matching_runcs_own_field_shape() {
    let Some((_bundle_dir, root_dir, cgroup_dir)) =
        create_and_start_with_real_cgroup("events-stats-test")
    else {
        return;
    };

    // Give the container a moment to actually burn some real CPU
    // (the bundle's own command sleeps, but process startup itself
    // already costs a few real, nonzero nanoseconds of usage_usec).
    std::thread::sleep(Duration::from_millis(50));

    let events = ocirun(root_dir.path(), &["events", "--stats", "events-stats-test"]);
    assert!(
        events.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&events.stderr)
    );
    let stdout = String::from_utf8_lossy(&events.stdout);
    let line = stdout.lines().next().expect("one real JSON line");
    let parsed: serde_json::Value = serde_json::from_str(line).expect("real JSON");

    assert_eq!(parsed["type"], serde_json::json!("stats"));
    assert_eq!(parsed["id"], serde_json::json!("events-stats-test"));

    // Cross-checked directly against the same real cgroup files
    // `ocirun update`'s own tests already read.
    let real_memory_max = std::fs::read_to_string(cgroup_dir.join("memory.max")).unwrap();
    assert_eq!(real_memory_max.trim(), "max", "no limit set by this test");
    assert_eq!(
        parsed["data"]["memory"]["usage"]["limit"],
        serde_json::json!(u64::MAX),
        "an unset memory.max (\"max\") must map to real runc's own identical u64::MAX sentinel"
    );

    let cpu_total = parsed["data"]["cpu"]["usage"]["total"]
        .as_u64()
        .expect("a real integer");
    assert!(
        cpu_total > 0,
        "a real, running process has nonzero cpu.stat usage_usec"
    );

    let mem_usage = parsed["data"]["memory"]["usage"]["usage"]
        .as_u64()
        .expect("a real integer");
    assert!(
        mem_usage > 0,
        "a real, running process has nonzero memory.current"
    );

    let pids_current = parsed["data"]["pids"]["current"]
        .as_u64()
        .expect("a real integer");
    assert!(
        pids_current >= 1,
        "at least the container's own init process"
    );

    cleanup(root_dir.path(), "events-stats-test", &cgroup_dir);
}

#[test]
fn events_without_stats_is_a_clear_not_yet_error() {
    let Some((_bundle_dir, root_dir, cgroup_dir)) =
        create_and_start_with_real_cgroup("events-no-stats-test")
    else {
        return;
    };

    let events = ocirun(root_dir.path(), &["events", "events-no-stats-test"]);
    assert!(!events.status.success());
    assert!(
        String::from_utf8_lossy(&events.stderr)
            .contains("periodic/OOM-notify mode isn't implemented"),
        "{}",
        String::from_utf8_lossy(&events.stderr)
    );

    cleanup(root_dir.path(), "events-no-stats-test", &cgroup_dir);
}

#[test]
fn events_stats_of_a_stopped_container_is_a_clear_error() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    write_bundle(bundle_dir.path(), &busybox, &["/bin/true"]);

    let create = ocirun_create(root_dir.path(), bundle_dir.path(), "events-stopped-test");
    assert!(create.status.success());
    let start = ocirun(root_dir.path(), &["start", "events-stopped-test"]);
    assert!(start.status.success());
    wait_for_status(
        root_dir.path(),
        "events-stopped-test",
        "stopped",
        Duration::from_secs(5),
    );

    let events = ocirun(
        root_dir.path(),
        &["events", "--stats", "events-stopped-test"],
    );
    assert!(!events.status.success());
    assert!(
        String::from_utf8_lossy(&events.stderr).contains("is not running"),
        "{}",
        String::from_utf8_lossy(&events.stderr)
    );

    let delete = ocirun(root_dir.path(), &["delete", "events-stopped-test"]);
    assert!(delete.status.success());
}
