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

/// `events --interval` (`docs/design/0539`), matching real `runc
/// events --interval` exactly: `--interval 0` is a real, immediate
/// error with real runc's own exact wording, verified live against a
/// real installed `runc 1.3.4` -- even on a genuinely running
/// container, and even though this project's own one-shot `--stats`
/// path never actually reads the parsed value for anything else.
#[test]
fn events_stats_interval_zero_is_a_clear_error() {
    let Some((_bundle_dir, root_dir, cgroup_dir)) =
        create_and_start_with_real_cgroup("events-interval-zero-test")
    else {
        return;
    };

    let events = ocirun(
        root_dir.path(),
        &[
            "events",
            "--interval",
            "0",
            "--stats",
            "events-interval-zero-test",
        ],
    );
    assert!(!events.status.success());
    assert!(
        String::from_utf8_lossy(&events.stderr)
            .contains("duration interval must be greater than 0"),
        "{}",
        String::from_utf8_lossy(&events.stderr)
    );

    cleanup(root_dir.path(), "events-interval-zero-test", &cgroup_dir);
}

/// An unparseable `--interval` value is also a real, immediate error
/// -- matching real runc's own equivalent flag-parse failure (checked
/// directly, `runc events --interval bogus --stats <ctr>`: `Incorrect
/// Usage: invalid value "bogus" for flag -interval: parse error`),
/// even though this project's own message wording isn't chased
/// byte-for-byte (a real, immediate error either way, not a silently
/// accepted garbage value).
#[test]
fn events_stats_unparseable_interval_is_a_clear_error() {
    let Some((_bundle_dir, root_dir, cgroup_dir)) =
        create_and_start_with_real_cgroup("events-interval-bogus-test")
    else {
        return;
    };

    let events = ocirun(
        root_dir.path(),
        &[
            "events",
            "--interval",
            "bogus",
            "--stats",
            "events-interval-bogus-test",
        ],
    );
    assert!(!events.status.success());
    assert!(
        String::from_utf8_lossy(&events.stderr).contains("invalid duration"),
        "{}",
        String::from_utf8_lossy(&events.stderr)
    );

    cleanup(root_dir.path(), "events-interval-bogus-test", &cgroup_dir);
}

/// A real, positive `--interval` behaves identically to the default
/// (no flag at all) -- the one-shot `--stats` report's own real
/// content never actually depends on the interval value, matching
/// real runc's own identical "validated, but never consumed on this
/// path" behavior.
#[test]
fn events_stats_with_a_valid_interval_behaves_identically_to_the_default() {
    let Some((_bundle_dir, root_dir, cgroup_dir)) =
        create_and_start_with_real_cgroup("events-interval-valid-test")
    else {
        return;
    };
    std::thread::sleep(Duration::from_millis(50));

    for interval in ["3s", "500ms", "1m"] {
        let events = ocirun(
            root_dir.path(),
            &[
                "events",
                "--interval",
                interval,
                "--stats",
                "events-interval-valid-test",
            ],
        );
        assert!(
            events.status.success(),
            "--interval {interval}: stderr: {}",
            String::from_utf8_lossy(&events.stderr)
        );
        let stdout = String::from_utf8_lossy(&events.stdout);
        let line = stdout.lines().next().expect("one real JSON line");
        let parsed: serde_json::Value = serde_json::from_str(line).expect("real JSON");
        assert_eq!(parsed["type"], serde_json::json!("stats"));
        assert_eq!(
            parsed["id"],
            serde_json::json!("events-interval-valid-test")
        );
    }

    cleanup(root_dir.path(), "events-interval-valid-test", &cgroup_dir);
}

/// `--interval`'s own validation runs *before* the "is the container
/// running" check -- matching real runc's own exact order (right
/// after confirming the container exists, before ever branching on
/// `--stats`) -- proven here against an already-*stopped* container,
/// which would otherwise report a completely different error
/// ("is not running") if the order were reversed.
#[test]
fn events_stats_interval_validation_runs_before_the_running_check() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    write_bundle(bundle_dir.path(), &busybox, &["/bin/true"]);

    let create = ocirun_create(root_dir.path(), bundle_dir.path(), "events-order-test");
    assert!(create.status.success());
    let start = ocirun(root_dir.path(), &["start", "events-order-test"]);
    assert!(start.status.success());
    wait_for_status(
        root_dir.path(),
        "events-order-test",
        "stopped",
        Duration::from_secs(5),
    );

    let events = ocirun(
        root_dir.path(),
        &["events", "--interval", "0", "--stats", "events-order-test"],
    );
    assert!(!events.status.success());
    assert!(
        String::from_utf8_lossy(&events.stderr)
            .contains("duration interval must be greater than 0"),
        "expected the interval error to take priority over the (also real) \"is not running\" \
         one, matching real runc's own exact validation order: {}",
        String::from_utf8_lossy(&events.stderr)
    );

    let delete = ocirun(root_dir.path(), &["delete", "events-order-test"]);
    assert!(delete.status.success());
}
