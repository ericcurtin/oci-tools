//! `ocirun create`/`start`/`kill`/`delete` integration tests: the
//! separate two-phase lifecycle (as opposed to `run`'s combined
//! create-and-start), exercised end to end against the actual built
//! `ocirun` binary and a real busybox rootfs.
//!
//! `create`'s own container process is deliberately left running in
//! the background once `create` returns (see `docs/design/0017`), so
//! every test here explicitly sets `Stdio::null()` on stdout/stderr for
//! the `create` invocation — the backgrounded container process
//! inherits whatever `create` had, and a real terminal/pipe (like the
//! one `Command::output()` otherwise sets up to capture output) would
//! never see EOF until *every* process holding a copy of it exits,
//! hanging this test process's own `output()` call for as long as the
//! container itself keeps running. Caught by hitting exactly that hang
//! once while manually verifying this against a real kernel, not
//! foreseen in advance.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use oci_tools_tests::{
    bin_path, busybox_path, ocirun, ocirun_create, state_status, wait_for_status, write_bundle,
};

/// Same real, reachable-`systemd --user`-session probe
/// `ocirun_run.rs`'s own cgroup test uses (see its own doc comment for
/// why a raw `cgroup.procs` write needs one).
fn systemd_user_scope_available() -> bool {
    Command::new("systemd-run")
        .args(["--user", "--scope", "--", "true"])
        .output()
        .is_ok_and(|out| out.status.success())
}

#[test]
fn create_start_kill_delete_lifecycle() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "sleep 30"]);

    let create = ocirun_create(root_dir.path(), bundle_dir.path(), "lifecycle-test");
    assert!(
        create.status.success(),
        "create failed: {}",
        String::from_utf8_lossy(&create.stderr)
    );
    assert_eq!(state_status(root_dir.path(), "lifecycle-test"), "created");

    let start = ocirun(root_dir.path(), &["start", "lifecycle-test"]);
    assert!(
        start.status.success(),
        "start failed: {}",
        String::from_utf8_lossy(&start.stderr)
    );
    assert_eq!(state_status(root_dir.path(), "lifecycle-test"), "running");

    // A plain SIGTERM (`kill`'s own default) is *silently ignored* here
    // by design, not a bug: the container process is pid 1 of its own
    // PID namespace, and the kernel ignores unhandled-default-action
    // signals sent to any pid-namespace's init unless it has installed
    // a handler (`man 7 pid_namespaces`) — busybox `sh` doesn't, and
    // neither does most real container ENTRYPOINTs without explicit
    // signal handling, so real `docker`/`podman`/`runc` hit the exact
    // same thing. Verified manually against a real kernel before
    // writing this test (see docs/design/0017) — asserting it's still
    // "running" here is intentional, not a placeholder.
    let term = ocirun(root_dir.path(), &["kill", "lifecycle-test"]);
    assert!(term.status.success());
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(state_status(root_dir.path(), "lifecycle-test"), "running");

    // SIGKILL cannot be ignored by anything, pid-namespace init
    // included.
    let kill = ocirun(root_dir.path(), &["kill", "lifecycle-test", "KILL"]);
    assert!(
        kill.status.success(),
        "kill failed: {}",
        String::from_utf8_lossy(&kill.stderr)
    );
    assert_eq!(
        wait_for_status(
            root_dir.path(),
            "lifecycle-test",
            "stopped",
            Duration::from_secs(5)
        ),
        "stopped"
    );

    let delete = ocirun(root_dir.path(), &["delete", "lifecycle-test"]);
    assert!(
        delete.status.success(),
        "delete failed: {}",
        String::from_utf8_lossy(&delete.stderr)
    );
    let after = ocirun(root_dir.path(), &["state", "lifecycle-test"]);
    assert!(
        !after.status.success(),
        "state should fail once deleted: {after:?}"
    );
}

#[test]
fn delete_without_force_refuses_a_running_container() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "sleep 30"]);

    ocirun_create(root_dir.path(), bundle_dir.path(), "no-force-test");
    let start = ocirun(root_dir.path(), &["start", "no-force-test"]);
    assert!(start.status.success());
    assert_eq!(state_status(root_dir.path(), "no-force-test"), "running");

    let refused = ocirun(root_dir.path(), &["delete", "no-force-test"]);
    assert!(
        !refused.status.success(),
        "delete without --force should refuse a running container"
    );

    let forced = ocirun(root_dir.path(), &["delete", "--force", "no-force-test"]);
    assert!(
        forced.status.success(),
        "delete --force failed: {}",
        String::from_utf8_lossy(&forced.stderr)
    );
    let after = ocirun(root_dir.path(), &["state", "no-force-test"]);
    assert!(!after.status.success());
}

#[test]
fn delete_a_never_started_container_kills_it_without_force() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "sleep 30"]);

    let create = ocirun_create(root_dir.path(), bundle_dir.path(), "never-started-test");
    assert!(create.status.success());
    assert_eq!(
        state_status(root_dir.path(), "never-started-test"),
        "created"
    );

    // Never `start`ed: the container process is still just blocked on
    // the exec fifo. `delete` without `--force` is still expected to
    // succeed here (matches real runc: a "created" container is always
    // deletable, killed outright since it never ran the user's
    // command).
    let delete = ocirun(root_dir.path(), &["delete", "never-started-test"]);
    assert!(
        delete.status.success(),
        "delete failed: {}",
        String::from_utf8_lossy(&delete.stderr)
    );
    let after = ocirun(root_dir.path(), &["state", "never-started-test"]);
    assert!(!after.status.success());
}

/// `delete` must remove the cgroup directory `create` migrated the
/// container's process into — the kernel does not do this on its own
/// (see `docs/design/0027`), and unlike `run` (which has the bundle
/// already loaded in the same process that created the cgroup),
/// `delete` is a wholly separate `ocirun` invocation that has to
/// re-derive the cgroup path from `state.bundle`'s own `config.json`.
#[test]
fn create_start_kill_delete_removes_the_cgroup_directory() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    if !systemd_user_scope_available() {
        eprintln!(
            "skipping: no reachable `systemd --user` session (systemd-run --user --scope failed)"
        );
        return;
    }

    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "sleep 30"]);

    let config_path = bundle_dir.path().join("config.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    let uid = rustix::process::getuid().as_raw();
    // Same reasoning as `ocirun_run.rs`'s own cgroup test: a sibling of
    // the carrier scope below, both direct children of the delegated
    // `app.slice`.
    let target = format!(
        "/user.slice/user-{uid}.slice/user@{uid}.service/app.slice/ocirun-lifecycle-cgroup-test-{}",
        std::process::id()
    );
    config["linux"]["cgroupsPath"] = serde_json::json!(target);
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
    let cgroup_dir = Path::new("/sys/fs/cgroup").join(target.trim_start_matches('/'));

    // Only `create` actually needs the delegated-cgroup carrier: it's
    // the one invocation that does the real `cgroup.procs` migration
    // (see `docs/design/0015`) — `start`/`kill`/`delete` don't touch
    // cgroups themselves (`delete`'s own `rmdir` only needs ordinary
    // write access to the parent directory, which any process running
    // as this uid already has under a fully-delegated subtree).
    let carrier_unit = format!(
        "ocirun-lifecycle-cgroup-test-carrier-{}.scope",
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
        .args(["create", "cgroup-cleanup-test", "--bundle"])
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
    assert_eq!(
        state_status(root_dir.path(), "cgroup-cleanup-test"),
        "created"
    );
    assert!(
        cgroup_dir.exists(),
        "cgroup directory {} should exist after create",
        cgroup_dir.display()
    );

    let start = ocirun(root_dir.path(), &["start", "cgroup-cleanup-test"]);
    assert!(start.status.success());
    assert_eq!(
        state_status(root_dir.path(), "cgroup-cleanup-test"),
        "running"
    );

    let kill = ocirun(root_dir.path(), &["kill", "cgroup-cleanup-test", "KILL"]);
    assert!(kill.status.success());
    assert_eq!(
        wait_for_status(
            root_dir.path(),
            "cgroup-cleanup-test",
            "stopped",
            Duration::from_secs(5)
        ),
        "stopped"
    );

    let delete = ocirun(root_dir.path(), &["delete", "cgroup-cleanup-test"]);
    assert!(
        delete.status.success(),
        "delete failed: {}",
        String::from_utf8_lossy(&delete.stderr)
    );
    assert!(
        !cgroup_dir.exists(),
        "cgroup directory {} should have been removed by delete",
        cgroup_dir.display()
    );
}
