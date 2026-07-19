//! `ocirun exec` integration tests: running an *additional* process
//! inside an already-running container (joining its existing
//! namespaces), exercised end to end against the actual built `ocirun`
//! binary and a real busybox rootfs, on top of the `create`/`start`
//! two-phase lifecycle `ocirun_lifecycle.rs` already covers.

use std::time::Duration;

use oci_tools_tests::{busybox_path, ocirun, ocirun_create, wait_for_status, write_bundle};

#[test]
fn exec_joins_the_running_containers_namespaces() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "sleep 30"]);

    let create = ocirun_create(root_dir.path(), bundle_dir.path(), "exec-test");
    assert!(
        create.status.success(),
        "create failed: {}",
        String::from_utf8_lossy(&create.stderr)
    );
    let start = ocirun(root_dir.path(), &["start", "exec-test"]);
    assert!(start.status.success());
    assert_eq!(
        wait_for_status(
            root_dir.path(),
            "exec-test",
            "running",
            Duration::from_secs(5)
        ),
        "running"
    );

    // `hostname` proves the exec'd process shares the container's own
    // UTS namespace (the default bundle sets hostname "ocirun" — see
    // `oci_spec_types::runtime::Spec::example`); `ps` proves it shares
    // the container's own PID namespace *and* rootfs (busybox's `ps`
    // only exists inside the container's own `/bin`), and that the
    // exec'd process gets a container-relative pid distinct from the
    // container's own init (which is always pid 1 in its own
    // namespace).
    let exec = ocirun(
        root_dir.path(),
        &["exec", "exec-test", "/bin/sh", "-c", "hostname && ps aux"],
    );
    let stdout = String::from_utf8_lossy(&exec.stdout).into_owned();
    assert!(
        exec.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&exec.stderr)
    );
    assert!(stdout.contains("ocirun"), "got stdout: {stdout:?}");
    assert!(
        stdout.contains("sleep 30"),
        "exec'd process should see the container's own init in `ps`: {stdout:?}"
    );
    // The container's own init is always pid 1 in its own namespace;
    // the exec'd process must be a *different* pid.
    assert!(
        !stdout
            .lines()
            .any(|l| l.trim_start().starts_with("1 ") && !l.contains("sleep 30")),
        "pid 1 should only be the container's own init: {stdout:?}"
    );

    // The container itself is unaffected: still running after `exec`
    // returns.
    assert_eq!(
        oci_tools_tests::state_status(root_dir.path(), "exec-test"),
        "running"
    );

    let delete = ocirun(root_dir.path(), &["delete", "--force", "exec-test"]);
    assert!(delete.status.success());
}

#[test]
fn exec_propagates_its_own_exit_code() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "sleep 30"]);

    ocirun_create(root_dir.path(), bundle_dir.path(), "exec-exit-test");
    ocirun(root_dir.path(), &["start", "exec-exit-test"]);
    wait_for_status(
        root_dir.path(),
        "exec-exit-test",
        "running",
        Duration::from_secs(5),
    );

    let exec = ocirun(
        root_dir.path(),
        &["exec", "exec-exit-test", "/bin/sh", "-c", "exit 9"],
    );
    assert_eq!(exec.status.code(), Some(9));

    // The main container process must still be running: `exec` failing
    // (a nonzero exit is expected/normal here) must not affect it.
    assert_eq!(
        oci_tools_tests::state_status(root_dir.path(), "exec-exit-test"),
        "running"
    );

    ocirun(root_dir.path(), &["delete", "--force", "exec-exit-test"]);
}

#[test]
fn exec_refuses_a_container_that_is_not_running() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "sleep 30"]);

    // Created but never started: blocked on the exec fifo, not running.
    ocirun_create(root_dir.path(), bundle_dir.path(), "exec-not-running-test");

    let exec = ocirun(
        root_dir.path(),
        &["exec", "exec-not-running-test", "/bin/true"],
    );
    assert!(
        !exec.status.success(),
        "exec should refuse a non-running container"
    );

    ocirun(
        root_dir.path(),
        &["delete", "--force", "exec-not-running-test"],
    );
}
