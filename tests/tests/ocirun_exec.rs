//! `ocirun exec` integration tests: running an *additional* process
//! inside an already-running container (joining its existing
//! namespaces), exercised end to end against the actual built `ocirun`
//! binary and a real busybox rootfs, on top of the `create`/`start`
//! two-phase lifecycle `ocirun_lifecycle.rs` already covers.

use std::time::Duration;

use oci_tools_tests::{
    bin_path, busybox_path, ocirun, ocirun_create, wait_for_status, write_bundle,
};

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
fn exec_cwd_flag_overrides_the_default_working_directory() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "sleep 30"]);
    std::fs::create_dir(bundle_dir.path().join("rootfs/tmp-cwd-test")).unwrap();

    ocirun_create(root_dir.path(), bundle_dir.path(), "exec-cwd-test");
    let start = ocirun(root_dir.path(), &["start", "exec-cwd-test"]);
    assert!(start.status.success());
    assert_eq!(
        wait_for_status(
            root_dir.path(),
            "exec-cwd-test",
            "running",
            Duration::from_secs(5)
        ),
        "running"
    );

    let exec = ocirun(
        root_dir.path(),
        &[
            "exec",
            "--cwd",
            "/tmp-cwd-test",
            "exec-cwd-test",
            "/bin/sh",
            "-c",
            "pwd",
        ],
    );
    assert!(
        exec.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&exec.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&exec.stdout).trim(),
        "/tmp-cwd-test"
    );

    ocirun(root_dir.path(), &["delete", "--force", "exec-cwd-test"]);
}

#[test]
fn exec_env_flag_appends_to_the_base_environment() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "sleep 30"]);

    ocirun_create(root_dir.path(), bundle_dir.path(), "exec-env-test");
    let start = ocirun(root_dir.path(), &["start", "exec-env-test"]);
    assert!(start.status.success());
    assert_eq!(
        wait_for_status(
            root_dir.path(),
            "exec-env-test",
            "running",
            Duration::from_secs(5)
        ),
        "running"
    );

    let exec = ocirun(
        root_dir.path(),
        &[
            "exec",
            "--env",
            "EXEC_TEST_VAR=exec-test-value",
            "exec-env-test",
            "/bin/sh",
            "-c",
            "echo \"$EXEC_TEST_VAR\"; echo \"got:$PATH\"",
        ],
    );
    assert!(
        exec.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&exec.stderr)
    );
    let stdout = String::from_utf8_lossy(&exec.stdout).into_owned();
    assert!(
        stdout.contains("exec-test-value"),
        "the --env var should be set: {stdout:?}"
    );
    assert!(
        stdout.contains("got:/usr/local/sbin"),
        "the container's own base PATH should still be set (appended to, not replaced): {stdout:?}"
    );

    ocirun(root_dir.path(), &["delete", "--force", "exec-env-test"]);
}

#[test]
fn exec_user_flag_to_root_succeeds() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "sleep 30"]);

    ocirun_create(root_dir.path(), bundle_dir.path(), "exec-user-root-test");
    let start = ocirun(root_dir.path(), &["start", "exec-user-root-test"]);
    assert!(start.status.success());
    assert_eq!(
        wait_for_status(
            root_dir.path(),
            "exec-user-root-test",
            "running",
            Duration::from_secs(5)
        ),
        "running"
    );

    let exec = ocirun(
        root_dir.path(),
        &[
            "exec",
            "--user",
            "0:0",
            "exec-user-root-test",
            "/bin/sh",
            "-c",
            "true",
        ],
    );
    assert!(
        exec.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&exec.stderr)
    );

    ocirun(
        root_dir.path(),
        &["delete", "--force", "exec-user-root-test"],
    );
}

/// `ocirun exec -g`/`--additional-gids`, matching real `runc exec
/// -g`/`--additional-gids` exactly (`crun exec` has no equivalent flag
/// at all, checked directly). Only verifies the flag is accepted and
/// composes correctly with `--user` and multiple repeated values --
/// this rootless test environment's own `/proc/self/setgroups` reads
/// `deny` (a real, environment-dependent kernel restriction real
/// `runc`/`crun` are equally subject to, not a bug — see
/// `identity::apply_supplementary_groups`'s own doc comment), so the
/// actual resulting group list isn't independently checkable here the
/// way it would be under a real, unprivileged-user-namespace-free
/// root install.
#[test]
fn exec_additional_gids_flag_is_accepted_and_composes_with_user() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "sleep 30"]);

    ocirun_create(root_dir.path(), bundle_dir.path(), "exec-gids-test");
    let start = ocirun(root_dir.path(), &["start", "exec-gids-test"]);
    assert!(start.status.success());
    assert_eq!(
        wait_for_status(
            root_dir.path(),
            "exec-gids-test",
            "running",
            Duration::from_secs(5)
        ),
        "running"
    );

    let exec = ocirun(
        root_dir.path(),
        &[
            "exec",
            "--user",
            "0:0",
            "-g",
            "100",
            "-g",
            "200",
            "exec-gids-test",
            "/bin/sh",
            "-c",
            "true",
        ],
    );
    assert!(
        exec.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&exec.stderr)
    );

    ocirun(root_dir.path(), &["delete", "--force", "exec-gids-test"]);
}

#[test]
fn exec_user_flag_to_a_non_root_uid_is_rejected() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "sleep 30"]);

    ocirun_create(root_dir.path(), bundle_dir.path(), "exec-user-nonroot-test");
    let start = ocirun(root_dir.path(), &["start", "exec-user-nonroot-test"]);
    assert!(start.status.success());
    assert_eq!(
        wait_for_status(
            root_dir.path(),
            "exec-user-nonroot-test",
            "running",
            Duration::from_secs(5)
        ),
        "running"
    );

    let exec = ocirun(
        root_dir.path(),
        &[
            "exec",
            "--user",
            "1000",
            "exec-user-nonroot-test",
            "/bin/sh",
            "-c",
            "true",
        ],
    );
    assert!(
        !exec.status.success(),
        "a non-root --user should be rejected: this rootless runtime only maps uid 0"
    );

    // The container itself must be unaffected by the rejected exec.
    assert_eq!(
        oci_tools_tests::state_status(root_dir.path(), "exec-user-nonroot-test"),
        "running"
    );

    ocirun(
        root_dir.path(),
        &["delete", "--force", "exec-user-nonroot-test"],
    );
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

/// `ocirun exec --preserve-fds` (0294), the identical real `runc
/// exec`/`crun exec --preserve-fds` semantics `ocirun run`/`create
/// --preserve-fds` (`0291`) already established for a container's own
/// *first* process, now also covering an *additional* one: by
/// default, every fd above stdio is closed before the exec'd process
/// ever runs (a real, previously-missing step this project's own
/// `exec.rs` never performed either, independently of `launch.rs`'s
/// own identical gap `0291` closed); `--preserve-fds N` keeps exactly
/// the first `N` of them instead.
///
/// Uses a real `pre_exec` closure on the *test's own* spawned `ocirun
/// exec` subprocess to `dup2` an already-open, real file onto exactly
/// fd 3 in that one child only -- the same technique (and the same
/// real POSIX `dup2(fd, fd)`-is-a-no-op-preserving-`FD_CLOEXEC` gotcha
/// guarded against) `ocirun_run.rs`'s own identical `--preserve-fds`
/// test already established.
#[test]
fn exec_preserve_fds_closes_extra_fds_by_default_but_keeps_them_with_the_flag() {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::process::CommandExt as _;

    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "sleep 30"]);

    let create = ocirun_create(root_dir.path(), bundle_dir.path(), "exec-preserve-fds-test");
    assert!(create.status.success(), "{create:?}");
    let start = ocirun(root_dir.path(), &["start", "exec-preserve-fds-test"]);
    assert!(start.status.success());
    assert_eq!(
        wait_for_status(
            root_dir.path(),
            "exec-preserve-fds-test",
            "running",
            Duration::from_secs(5)
        ),
        "running"
    );

    let run_with_fd3_open = |extra_args: &[&str]| -> std::process::Output {
        let marker = tempfile::NamedTempFile::new().unwrap();
        let raw_fd = marker.as_file().as_raw_fd();
        let mut cmd = std::process::Command::new(bin_path("ocirun"));
        cmd.arg("--root")
            .arg(root_dir.path())
            .arg("exec")
            .args(extra_args)
            .args([
                "exec-preserve-fds-test",
                "/bin/sh",
                "-c",
                "test -e /proc/self/fd/3 && echo fd3-present || echo fd3-absent",
            ])
            .env_remove("OCI_TOOLS_LOG");
        // SAFETY: only calls `dup2(2)`/`fcntl(2)` (both async-signal-
        // safe, no allocation) in the forked-but-not-yet-exec'd child,
        // only ever affecting that child's own fd table -- see
        // `ocirun_run.rs`'s own identical test for exactly why the
        // `fcntl` call is needed too.
        #[allow(unsafe_code)]
        unsafe {
            cmd.pre_exec(move || {
                if libc::dup2(raw_fd, 3) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let out = cmd.output().expect("failed to spawn ocirun exec");
        drop(marker);
        out
    };

    let without_flag = run_with_fd3_open(&[]);
    assert!(
        without_flag.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&without_flag.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&without_flag.stdout).trim(),
        "fd3-absent",
        "fd 3 must be closed by default, matching real runc/crun: {without_flag:?}"
    );

    let with_flag = run_with_fd3_open(&["--preserve-fds", "1"]);
    assert!(
        with_flag.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&with_flag.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&with_flag.stdout).trim(),
        "fd3-present",
        "fd 3 must be preserved with --preserve-fds 1: {with_flag:?}"
    );

    ocirun(
        root_dir.path(),
        &["delete", "--force", "exec-preserve-fds-test"],
    );
}

/// `--preserve-fds N` on `exec` fails fast, before ever joining the
/// container's own namespaces at all, if fewer than `N` fds are
/// actually open starting at fd 3 -- matching real runc's own
/// identical upfront `Faccessat` check `ocirun run`/`create
/// --preserve-fds` (`0291`) already established, reused verbatim here
/// via the same shared `verify_preserve_fds` helper.
#[test]
fn exec_preserve_fds_rejects_a_claim_with_no_matching_open_fd() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "sleep 30"]);

    let create = ocirun_create(
        root_dir.path(),
        bundle_dir.path(),
        "exec-preserve-fds-reject-test",
    );
    assert!(create.status.success(), "{create:?}");
    let start = ocirun(root_dir.path(), &["start", "exec-preserve-fds-reject-test"]);
    assert!(start.status.success());
    assert_eq!(
        wait_for_status(
            root_dir.path(),
            "exec-preserve-fds-reject-test",
            "running",
            Duration::from_secs(5)
        ),
        "running"
    );

    let out = ocirun(
        root_dir.path(),
        &[
            "exec",
            "--preserve-fds",
            "5",
            "exec-preserve-fds-reject-test",
            "/bin/true",
        ],
    );
    assert!(!out.status.success(), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("is not open in this process"),
        "{out:?}"
    );

    ocirun(
        root_dir.path(),
        &["delete", "--force", "exec-preserve-fds-reject-test"],
    );
}
