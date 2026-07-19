//! `ocirun ps` integration tests: matches real `runc ps` exactly
//! (`~/git/runc/ps.go`) — every real pid in a container's own cgroup,
//! either as a bare JSON array (`--format json`) or filtered into the
//! real host `ps` binary's own table output (`--format table`, the
//! default). Needs a real, delegated `systemd --user` cgroup subtree
//! to actually exercise (same reasoning/setup
//! `ocirun_lifecycle.rs`'s own `create_start_kill_delete_removes_the_
//! cgroup_directory` test already established) — skips cleanly where
//! unavailable.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use oci_tools_tests::{
    bin_path, busybox_path, ocirun, ocirun_create, wait_for_status, write_bundle,
};

/// Same real, reachable-`systemd --user`-session probe
/// `ocirun_lifecycle.rs`'s own cgroup test uses.
fn systemd_user_scope_available() -> bool {
    Command::new("systemd-run")
        .args(["--user", "--scope", "--", "true"])
        .output()
        .is_ok_and(|out| out.status.success())
}

/// Set up a real, running container with a real delegated cgroup
/// subtree (needed for `ocirun ps` to have anything real to report at
/// all), returning `(bundle_dir, root_dir, cgroup_dir)` — callers are
/// responsible for `kill`+`delete`ing `id` afterward.
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
        "/user.slice/user-{uid}.slice/user@{uid}.service/app.slice/ocirun-ps-test-{id}-{}",
        std::process::id()
    );
    config["linux"]["cgroupsPath"] = serde_json::json!(target);
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
    let cgroup_dir = Path::new("/sys/fs/cgroup").join(target.trim_start_matches('/'));

    let carrier_unit = format!("ocirun-ps-test-carrier-{id}-{}.scope", std::process::id());
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
fn ps_json_format_reports_the_real_container_pid() {
    let Some((_bundle_dir, root_dir, cgroup_dir)) = create_and_start_with_real_cgroup("json-test")
    else {
        return;
    };

    let real_pid: i64 = {
        let state = ocirun(root_dir.path(), &["state", "json-test"]);
        let json: serde_json::Value = serde_json::from_slice(&state.stdout).unwrap();
        json["pid"].as_i64().unwrap()
    };

    let ps = ocirun(root_dir.path(), &["ps", "json-test", "--format", "json"]);
    assert!(
        ps.status.success(),
        "ps failed: {}",
        String::from_utf8_lossy(&ps.stderr)
    );
    let pids: Vec<i64> = serde_json::from_slice(&ps.stdout).expect("ps --format json output");
    assert!(
        pids.contains(&real_pid),
        "expected the real container pid {real_pid} among {pids:?}"
    );

    cleanup(root_dir.path(), "json-test", &cgroup_dir);
}

#[test]
fn ps_table_format_shows_the_real_containers_own_command() {
    let Some((_bundle_dir, root_dir, cgroup_dir)) = create_and_start_with_real_cgroup("table-test")
    else {
        return;
    };

    let ps = ocirun(root_dir.path(), &["ps", "table-test"]);
    assert!(
        ps.status.success(),
        "ps failed: {}",
        String::from_utf8_lossy(&ps.stderr)
    );
    let stdout = String::from_utf8_lossy(&ps.stdout);
    let mut lines = stdout.lines();
    let header = lines.next().expect("a header line");
    assert!(
        header.contains("PID"),
        "header should have a PID column: {header:?}"
    );
    assert!(
        stdout.contains("sleep 30"),
        "expected the container's own real command in the (default `-ef`) table output: {stdout:?}"
    );

    cleanup(root_dir.path(), "table-test", &cgroup_dir);
}

#[test]
fn ps_passes_extra_arguments_straight_through_to_the_real_ps_binary() {
    let Some((_bundle_dir, root_dir, cgroup_dir)) = create_and_start_with_real_cgroup("aux-test")
    else {
        return;
    };

    // Real `ps -ef`'s own header uses `UID`; real `ps aux`'s own uses
    // `USER` -- a real, observable difference proving the extra
    // argument genuinely reached the real host `ps` binary, not just
    // accepted and ignored.
    let ps = ocirun(root_dir.path(), &["ps", "aux-test", "aux"]);
    assert!(
        ps.status.success(),
        "ps failed: {}",
        String::from_utf8_lossy(&ps.stderr)
    );
    let stdout = String::from_utf8_lossy(&ps.stdout);
    let header = stdout.lines().next().expect("a header line");
    assert!(
        header.contains("USER"),
        "expected `ps aux`'s own USER column, got: {header:?}"
    );
    assert!(
        stdout.contains("sleep 30"),
        "expected the container's own real command: {stdout:?}"
    );

    cleanup(root_dir.path(), "aux-test", &cgroup_dir);
}

#[test]
fn ps_without_a_cgroup_reports_no_processes_not_an_error() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    // Deliberately no `cgroupsPath` set at all -- `write_bundle`'s own
    // default `ocirun spec --rootless` output never sets one.
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "sleep 30"]);

    let create = ocirun_create(root_dir.path(), bundle_dir.path(), "no-cgroup-ps-test");
    assert!(create.status.success());
    let start = ocirun(root_dir.path(), &["start", "no-cgroup-ps-test"]);
    assert!(start.status.success());

    let json = ocirun(
        root_dir.path(),
        &["ps", "no-cgroup-ps-test", "--format", "json"],
    );
    assert!(json.status.success());
    let pids: Vec<i64> = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(pids, Vec::<i64>::new());

    let table = ocirun(root_dir.path(), &["ps", "no-cgroup-ps-test"]);
    assert!(table.status.success());
    let stdout = String::from_utf8_lossy(&table.stdout);
    assert_eq!(
        stdout.lines().count(),
        1,
        "only the header line, no real process rows: {stdout:?}"
    );

    let kill = ocirun(root_dir.path(), &["kill", "no-cgroup-ps-test", "KILL"]);
    assert!(kill.status.success());
    wait_for_status(
        root_dir.path(),
        "no-cgroup-ps-test",
        "stopped",
        Duration::from_secs(5),
    );
    ocirun(root_dir.path(), &["delete", "no-cgroup-ps-test"]);
}

#[test]
fn ps_on_an_unknown_container_is_a_real_error() {
    let root_dir = tempfile::tempdir().unwrap();
    let ps = ocirun(root_dir.path(), &["ps", "does-not-exist"]);
    assert!(!ps.status.success());
}

#[test]
fn ps_rejects_an_invalid_format_option() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "sleep 30"]);
    let create = ocirun_create(root_dir.path(), bundle_dir.path(), "bad-format-test");
    assert!(create.status.success());

    let ps = ocirun(
        root_dir.path(),
        &["ps", "bad-format-test", "--format", "yaml"],
    );
    assert!(!ps.status.success());
    let stderr = String::from_utf8_lossy(&ps.stderr);
    assert!(stderr.contains("invalid format option"), "{stderr}");

    let kill = ocirun(root_dir.path(), &["kill", "bad-format-test", "KILL"]);
    assert!(kill.status.success());
    ocirun(root_dir.path(), &["delete", "bad-format-test"]);
}
