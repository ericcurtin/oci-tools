//! `ocirun exec` integration tests: running an *additional* process
//! inside an already-running container (joining its existing
//! namespaces), exercised end to end against the actual built `ocirun`
//! binary and a real busybox rootfs, on top of the `create`/`start`
//! two-phase lifecycle `ocirun_lifecycle.rs` already covers.

use std::io::Write as _;
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

/// `ocirun exec --cap`/`-c` (`docs/design/0363`): additive on top of
/// the container's own already-granted capability set, matching real
/// `runc exec --cap` exactly (see `Command::Exec::cap`'s own doc
/// comment for the exact real, checked-directly bit math this
/// mirrors). `ocirun spec`'s own default bounding/effective/permitted
/// set is exactly `CAP_KILL` (bit 5) | `CAP_NET_BIND_SERVICE` (bit
/// 10) | `CAP_AUDIT_WRITE` (bit 29) = `0x20000420` (the same real
/// bitmask `run_applies_the_default_capability_set_and_no_new_
/// privileges` in `ocirun_run.rs` already established); adding
/// `CAP_NET_ADMIN` (bit 12 = `0x1000`) via `--cap` must set exactly
/// that one extra bit, on all three sets, while `CapAmb` stays `0`
/// (the container's own default `inheritable` is empty, so ambient
/// stays ineligible — see the same doc comment).
#[test]
fn exec_cap_adds_a_capability_on_top_of_the_containers_own_default_set() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "sleep 30"]);

    let create = ocirun_create(root_dir.path(), bundle_dir.path(), "exec-cap-test");
    assert!(create.status.success(), "{create:?}");
    let start = ocirun(root_dir.path(), &["start", "exec-cap-test"]);
    assert!(start.status.success());
    assert_eq!(
        wait_for_status(
            root_dir.path(),
            "exec-cap-test",
            "running",
            Duration::from_secs(5)
        ),
        "running"
    );

    let grep_caps = r#"grep -E "^(CapPrm|CapEff|CapBnd|CapAmb):" /proc/self/status"#;

    let without_cap = ocirun(
        root_dir.path(),
        &["exec", "exec-cap-test", "/bin/sh", "-c", grep_caps],
    );
    assert!(without_cap.status.success(), "{without_cap:?}");
    assert_eq!(
        String::from_utf8_lossy(&without_cap.stdout).trim(),
        "CapPrm:\t0000000020000420\nCapEff:\t0000000020000420\nCapBnd:\t0000000020000420\nCapAmb:\t0000000000000000",
        "no --cap given should leave the container's own default set untouched"
    );

    let with_cap = ocirun(
        root_dir.path(),
        &[
            "exec",
            "--cap",
            "CAP_NET_ADMIN",
            "exec-cap-test",
            "/bin/sh",
            "-c",
            grep_caps,
        ],
    );
    assert!(with_cap.status.success(), "{with_cap:?}");
    assert_eq!(
        String::from_utf8_lossy(&with_cap.stdout).trim(),
        "CapPrm:\t0000000020001420\nCapEff:\t0000000020001420\nCapBnd:\t0000000020001420\nCapAmb:\t0000000000000000",
        "--cap CAP_NET_ADMIN should add exactly bit 12 on top of the default set, ambient \
         staying 0 since the container's own inheritable set is empty"
    );

    ocirun(root_dir.path(), &["delete", "--force", "exec-cap-test"]);
}

/// `ocirun exec --no-new-privs` (matching real `runc exec --no-new-
/// privs`/`crun exec --no-new-privs` exactly, checked directly): a
/// plain, bare boolean flag that forces the exec'd process's own
/// `NoNewPrivs` to `1`, even when the container's own declared
/// `process.noNewPrivileges` is `false` -- not given at all (the
/// default) leaves it inheriting that same declared value unchanged.
#[test]
fn exec_no_new_privs_flag_forces_it_on_regardless_of_the_containers_own_declared_value() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "sleep 30"]);
    let config_path = bundle_dir.path().join("config.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["process"]["noNewPrivileges"] = serde_json::json!(false);
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();

    let create = ocirun_create(root_dir.path(), bundle_dir.path(), "exec-no-new-privs-test");
    assert!(create.status.success(), "{create:?}");
    let start = ocirun(root_dir.path(), &["start", "exec-no-new-privs-test"]);
    assert!(start.status.success());
    assert_eq!(
        wait_for_status(
            root_dir.path(),
            "exec-no-new-privs-test",
            "running",
            Duration::from_secs(5)
        ),
        "running"
    );

    let grep_nnp = r#"grep -E "^NoNewPrivs:" /proc/self/status"#;

    let without_flag = ocirun(
        root_dir.path(),
        &["exec", "exec-no-new-privs-test", "/bin/sh", "-c", grep_nnp],
    );
    assert!(without_flag.status.success(), "{without_flag:?}");
    assert_eq!(
        String::from_utf8_lossy(&without_flag.stdout).trim(),
        "NoNewPrivs:\t0",
        "no --no-new-privs given should inherit the container's own declared false value"
    );

    let with_flag = ocirun(
        root_dir.path(),
        &[
            "exec",
            "--no-new-privs",
            "exec-no-new-privs-test",
            "/bin/sh",
            "-c",
            grep_nnp,
        ],
    );
    assert!(with_flag.status.success(), "{with_flag:?}");
    assert_eq!(
        String::from_utf8_lossy(&with_flag.stdout).trim(),
        "NoNewPrivs:\t1",
        "--no-new-privs should force it on regardless of the container's own declared value"
    );

    ocirun(
        root_dir.path(),
        &["delete", "--force", "exec-no-new-privs-test"],
    );
}

/// Same real, reachable-`systemd --user`-session probe
/// `ocirun_lifecycle.rs`'s own pause/resume test uses (see its own
/// doc comment for why a real cgroup -- required by `pause`, unlike
/// this file's other tests -- needs one here too).
fn systemd_user_scope_available() -> bool {
    std::process::Command::new("systemd-run")
        .args(["--user", "--scope", "--", "true"])
        .output()
        .is_ok_and(|out| out.status.success())
}

/// `ocirun exec --ignore-paused` (`docs/design/0363`): a real,
/// genuinely-`Paused` container refuses `exec` by default, matching
/// this project's own pre-existing behavior, but `--ignore-paused`
/// (matching real `runc exec --ignore-paused` exactly) lets it through
/// anyway. Needs a real cgroup for `pause` to actually act on --
/// unlike this file's other tests, which never touch one at all --
/// via the same real, delegated-cgroup carrier-scope setup
/// `ocirun_lifecycle.rs`'s own `pause_freezes_and_resume_thaws_a_real_
/// running_containers_own_cpu_usage` already established (see its own
/// doc comment for exactly why `create` alone needs the carrier).
#[test]
fn exec_ignore_paused_allows_exec_into_a_genuinely_paused_container() {
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
    let target = format!(
        "/user.slice/user-{uid}.slice/user@{uid}.service/app.slice/ocirun-exec-ignore-paused-{}",
        std::process::id()
    );
    config["linux"]["cgroupsPath"] = serde_json::json!(target);
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();

    let carrier_unit = format!(
        "ocirun-exec-ignore-paused-carrier-{}.scope",
        std::process::id()
    );
    let create = std::process::Command::new("systemd-run")
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
        .args(["create", "exec-ignore-paused-test", "--bundle"])
        .arg(bundle_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .expect("failed to spawn systemd-run");
    assert!(
        create.status.success(),
        "create failed: {}",
        String::from_utf8_lossy(&create.stderr)
    );

    let start = ocirun(root_dir.path(), &["start", "exec-ignore-paused-test"]);
    assert!(start.status.success());
    assert_eq!(
        wait_for_status(
            root_dir.path(),
            "exec-ignore-paused-test",
            "running",
            Duration::from_secs(5)
        ),
        "running"
    );

    let pause = ocirun(root_dir.path(), &["pause", "exec-ignore-paused-test"]);
    assert!(
        pause.status.success(),
        "pause failed: {}",
        String::from_utf8_lossy(&pause.stderr)
    );
    assert_eq!(
        wait_for_status(
            root_dir.path(),
            "exec-ignore-paused-test",
            "paused",
            Duration::from_secs(5)
        ),
        "paused"
    );

    let refused = ocirun(
        root_dir.path(),
        &["exec", "exec-ignore-paused-test", "/bin/true"],
    );
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("paused"),
        "{refused:?}"
    );

    let allowed = ocirun(
        root_dir.path(),
        &[
            "exec",
            "--ignore-paused",
            "exec-ignore-paused-test",
            "/bin/true",
        ],
    );
    assert!(
        allowed.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&allowed.stderr)
    );

    let resume = ocirun(root_dir.path(), &["resume", "exec-ignore-paused-test"]);
    assert!(resume.status.success(), "{resume:?}");
    ocirun(
        root_dir.path(),
        &["delete", "--force", "exec-ignore-paused-test"],
    );
}

/// A regression guard for the "no behavior change for `ocirun`" side
/// of 0385: `ocirun exec` has no `-i`/interactive concept at all
/// (matching real `runc exec`/`crun exec` exactly, checked directly
/// against both installed binaries' own `--help` -- neither has one),
/// so it must keep forwarding whatever stdin its own caller has
/// unconditionally, exactly as it always has.
#[test]
fn exec_always_forwards_real_stdin_unconditionally() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "sleep 30"]);

    let create = ocirun_create(root_dir.path(), bundle_dir.path(), "exec-stdin-test");
    assert!(create.status.success(), "{create:?}");
    let start = ocirun(root_dir.path(), &["start", "exec-stdin-test"]);
    assert!(start.status.success());
    assert_eq!(
        wait_for_status(
            root_dir.path(),
            "exec-stdin-test",
            "running",
            Duration::from_secs(5)
        ),
        "running"
    );

    let mut child = std::process::Command::new(bin_path("ocirun"))
        .args(["--root"])
        .arg(root_dir.path())
        .args([
            "exec",
            "exec-stdin-test",
            "/bin/sh",
            "-c",
            "if read -t 5 line; then echo GOT:$line; else echo NOINPUT; fi",
        ])
        .env_remove("OCI_TOOLS_LOG")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn ocirun exec");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"hello-from-host-stdin\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "GOT:hello-from-host-stdin",
        "ocirun exec must always forward real host stdin, unconditionally"
    );

    ocirun(root_dir.path(), &["delete", "--force", "exec-stdin-test"]);
}

/// `ocirun exec --pid-file` (`docs/design/0387`), matching real `runc
/// exec --pid-file`/`crun exec --pid-file` exactly (checked directly,
/// `~/git/runc/utils_linux.go`'s own `createPidFile(r.pidFile,
/// process)` call in its `runner.run`; `~/git/crun/src/exec.c`'s own
/// identical `pid_file` option): the file must contain the real,
/// host-visible pid of the exec'd process *itself* -- proven here not
/// just by reading it back, but by sending it a real `SIGKILL`
/// directly and observing the exec'd `ocirun exec` process's own exit
/// code become the matching `128 + SIGKILL` code (`process::exit_
/// code_from_wait_status`), which could only happen if the pid in the
/// file really is the one this project's own relay is blocked waiting
/// on. The default bundle already joins a PID namespace (see `exec_
/// joins_the_running_containers_namespaces`'s own doc comment), so
/// this single test already exercises the PID-namespace-relay branch
/// (`ExecSetup::run`'s `needs_pid_relay` path, reporting the *inner*
/// fork's own pid, never the outer relay's) -- the only branch that
/// actually needs its own dedicated coverage, since the no-relay
/// branch is just `rustix::process::getpid()` of the process about to
/// `exec` itself, with nothing else in between that could plausibly
/// report the wrong value.
#[test]
fn exec_pid_file_writes_the_real_pid_of_the_exec_process() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    let pid_file = bundle_dir.path().join("exec.pid");
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "sleep 30"]);

    let create = ocirun_create(root_dir.path(), bundle_dir.path(), "exec-pid-file-test");
    assert!(create.status.success(), "{create:?}");
    let start = ocirun(root_dir.path(), &["start", "exec-pid-file-test"]);
    assert!(start.status.success());
    assert_eq!(
        wait_for_status(
            root_dir.path(),
            "exec-pid-file-test",
            "running",
            Duration::from_secs(5)
        ),
        "running"
    );

    let exec = std::process::Command::new(bin_path("ocirun"))
        .args(["--root"])
        .arg(root_dir.path())
        .args(["exec", "--pid-file"])
        .arg(&pid_file)
        .args(["exec-pid-file-test", "/bin/sh", "-c", "sleep 30"])
        .env_remove("OCI_TOOLS_LOG")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn ocirun exec");

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !pid_file.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(pid_file.exists(), "--pid-file was never written");

    let file_content = std::fs::read_to_string(&pid_file).unwrap();
    let exec_pid: i32 = file_content.trim().parse().unwrap_or_else(|e| {
        panic!("--pid-file content {file_content:?} not a plain decimal pid: {e}")
    });
    assert_eq!(
        file_content,
        exec_pid.to_string(),
        "--pid-file's own content should be exactly the bare decimal pid, no trailing newline, \
         matching real runc's own createPidFile"
    );

    // Prove `exec_pid` is really the exec'd `sleep 30` process itself,
    // not this project's own outer relay pid: killing it directly
    // must make `ocirun exec`'s own wait immediately observe that
    // exact signal death.
    //
    // SAFETY: `kill(2)` on a plain `i32` pid has no memory-safety
    // requirements of its own.
    #[allow(unsafe_code)]
    let kill_result = unsafe { libc::kill(exec_pid, libc::SIGKILL) };
    assert_eq!(
        kill_result,
        0,
        "SIGKILL to the reported pid should succeed: {}",
        std::io::Error::last_os_error()
    );

    let out = exec
        .wait_with_output()
        .expect("failed to wait for ocirun exec");
    assert_eq!(
        out.status.code(),
        Some(128 + libc::SIGKILL),
        "ocirun exec's own exit code should reflect the exec'd process's real SIGKILL death: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The container itself must be unaffected: still running after the
    // exec'd process was killed directly.
    assert_eq!(
        oci_tools_tests::state_status(root_dir.path(), "exec-pid-file-test"),
        "running"
    );

    ocirun(
        root_dir.path(),
        &["delete", "--force", "exec-pid-file-test"],
    );
}

/// `ocirun exec --process`/`-p` (matching real `runc exec --process`/
/// `crun exec --process` exactly, checked directly, `~/git/runc/
/// exec.go`'s own `getProcess`): the entire process specification
/// comes from the given JSON file instead of `COMMAND`/`--user`/
/// `--cwd`/`--env`/`--no-new-privs`, all of which are given here too
/// (deliberately mismatched from the file's own values) to prove they
/// are genuinely ignored, not merged.
#[test]
fn exec_process_flag_reads_the_entire_spec_from_a_json_file_ignoring_other_flags() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "sleep 30"]);

    let create = ocirun_create(root_dir.path(), bundle_dir.path(), "exec-process-test");
    assert!(create.status.success(), "{create:?}");
    let start = ocirun(root_dir.path(), &["start", "exec-process-test"]);
    assert!(start.status.success());
    assert_eq!(
        wait_for_status(
            root_dir.path(),
            "exec-process-test",
            "running",
            Duration::from_secs(5)
        ),
        "running"
    );

    // `cwd` deliberately set to `/bin` rather than `/` (the default
    // every bundle's own process already declares, `Spec::example`)
    // -- `/bin` genuinely exists in this test's own minimal busybox
    // rootfs (`write_bundle` creates it), and differs from the
    // default, so a correct `pwd` here can only mean the JSON file's
    // own `cwd` genuinely took effect.
    let process_json = serde_json::json!({
        "user": {"uid": 0, "gid": 0},
        "args": ["/bin/sh", "-c", "pwd; env | grep ^MARKER=; grep -E \"^NoNewPrivs:\" /proc/self/status"],
        "env": ["MARKER=from-process-json"],
        "cwd": "/bin",
        "noNewPrivileges": true
    });
    let process_path = bundle_dir.path().join("process.json");
    std::fs::write(
        &process_path,
        serde_json::to_vec_pretty(&process_json).unwrap(),
    )
    .unwrap();

    let exec = ocirun(
        root_dir.path(),
        &[
            "exec",
            "--process",
            process_path.to_str().unwrap(),
            // Deliberately mismatched, must be ignored entirely.
            "--cwd",
            "/",
            "--env",
            "SHOULD_NOT_APPEAR=1",
            "exec-process-test",
        ],
    );
    assert!(
        exec.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&exec.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&exec.stdout),
        "/bin\nMARKER=from-process-json\nNoNewPrivs:\t1\n"
    );

    ocirun(root_dir.path(), &["delete", "--force", "exec-process-test"]);
}

/// Real `runc exec`/`crun exec` both require a `COMMAND` when
/// `--process` isn't given — matching that exactly, now that `exec`'s
/// own `args` positional is no longer unconditionally `required` at
/// the clap level (needed to make `--process` alone valid).
#[test]
fn exec_with_neither_process_nor_a_command_is_a_clear_error() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "sleep 30"]);

    let create = ocirun_create(root_dir.path(), bundle_dir.path(), "exec-no-args-test");
    assert!(create.status.success(), "{create:?}");
    let start = ocirun(root_dir.path(), &["start", "exec-no-args-test"]);
    assert!(start.status.success());
    assert_eq!(
        wait_for_status(
            root_dir.path(),
            "exec-no-args-test",
            "running",
            Duration::from_secs(5)
        ),
        "running"
    );

    let exec = ocirun(root_dir.path(), &["exec", "exec-no-args-test"]);
    assert!(!exec.status.success());
    assert!(
        String::from_utf8_lossy(&exec.stderr).contains("exec args cannot be empty"),
        "{exec:?}"
    );

    ocirun(root_dir.path(), &["delete", "--force", "exec-no-args-test"]);
}

/// `ocirun exec --detach`/`-d` (`docs/design/0533`), matching real
/// `runc exec --detach`/`-d`/`crun exec --detach`/`-d` exactly: the
/// invocation itself returns success (exit `0`) as soon as the
/// exec'd process is under way, well before the (deliberately much
/// longer) command it started actually finishes — proven here by a
/// real wall-clock bound on the `exec --detach` call itself, then
/// polling for the detached command's own real, delayed side effect
/// (a marker file, written only after its own longer sleep) to prove
/// it really did keep running in the background rather than this
/// project's own process having silently killed it early. The
/// container itself stays completely unaffected throughout — no
/// separate "keeper" process is needed for this (unlike `ocirun run
/// --detach`, `0375`), since a detached `exec`'d process is simply
/// left to be reparented to the nearest subreaper/`PID 1` once this
/// invocation exits, the exact real mechanism both reference runtimes
/// rely on too.
#[test]
fn exec_detach_returns_immediately_without_waiting_for_the_command_to_finish() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    let marker = bundle_dir.path().join("rootfs/marker.txt");
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "sleep 30"]);

    let create = ocirun_create(root_dir.path(), bundle_dir.path(), "exec-detach-test");
    assert!(create.status.success(), "{create:?}");
    let start = ocirun(root_dir.path(), &["start", "exec-detach-test"]);
    assert!(start.status.success());
    assert_eq!(
        wait_for_status(
            root_dir.path(),
            "exec-detach-test",
            "running",
            Duration::from_secs(5)
        ),
        "running"
    );

    // Deliberately *not* the `ocirun` helper above (which captures
    // stdout/stderr via a real pipe through `Command::output()`): the
    // detached grandchild below inherits whatever stdio this
    // invocation itself has, so a captured pipe would never see `EOF`
    // -- and `output()` would never return -- until *that* process
    // *also* exits, ~3s later, hiding the very thing this test is
    // trying to prove. The exact same real hazard `ocirun_create`'s
    // own doc comment above already documents for an analogous case;
    // `Stdio::null()` (not a pipe at all) sidesteps it completely.
    let started = std::time::Instant::now();
    let exec = std::process::Command::new(bin_path("ocirun"))
        .arg("--root")
        .arg(root_dir.path())
        .args([
            "exec",
            "--detach",
            "exec-detach-test",
            "/bin/sh",
            "-c",
            "sleep 3; echo done > /marker.txt",
        ])
        .env_remove("OCI_TOOLS_LOG")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("failed to spawn ocirun exec --detach");
    let elapsed = started.elapsed();
    assert!(exec.success(), "ocirun exec --detach failed: {exec:?}");
    assert!(
        elapsed < Duration::from_secs(2),
        "--detach should return almost immediately, not block on the full 3s sleep: {elapsed:?}"
    );
    // The detached command hasn't had time to write its own marker
    // yet -- proving this invocation really didn't wait for it.
    assert!(
        !marker.exists(),
        "marker should not exist yet immediately after --detach returns"
    );

    // The container itself is unaffected: still running.
    assert_eq!(
        oci_tools_tests::state_status(root_dir.path(), "exec-detach-test"),
        "running"
    );

    // The detached command really did keep running in the background
    // and eventually wrote its own marker.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !marker.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        marker.exists(),
        "the detached command never wrote its own marker file"
    );
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "done\n");

    ocirun(root_dir.path(), &["delete", "--force", "exec-detach-test"]);
}

/// `--detach` composes with `--pid-file`, writing it *before*
/// returning -- matching real runc's own exact order (`~/git/runc/
/// utils_linux.go`'s own `runner.run`: `createPidFile` happens, then
/// `if detach { return 0, nil }`), the same order [`exec_reporting_
/// pid`]'s own pre-existing `on_pid`-then-return-on-detach sequence
/// already gives for free, needing no special-casing at all.
#[test]
fn exec_detach_still_writes_the_pid_file_before_returning() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    let pid_file = bundle_dir.path().join("exec-detach.pid");
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "sleep 30"]);

    let create = ocirun_create(
        root_dir.path(),
        bundle_dir.path(),
        "exec-detach-pidfile-test",
    );
    assert!(create.status.success(), "{create:?}");
    let start = ocirun(root_dir.path(), &["start", "exec-detach-pidfile-test"]);
    assert!(start.status.success());
    assert_eq!(
        wait_for_status(
            root_dir.path(),
            "exec-detach-pidfile-test",
            "running",
            Duration::from_secs(5)
        ),
        "running"
    );

    // `Stdio::null()` for the same real reason the timing test above
    // documents: the detached grandchild otherwise inherits a real
    // pipe that would never see `EOF` until it too exits.
    let exec = std::process::Command::new(bin_path("ocirun"))
        .args(["--root"])
        .arg(root_dir.path())
        .args(["exec", "--detach", "--pid-file"])
        .arg(&pid_file)
        .args(["exec-detach-pidfile-test", "/bin/sh", "-c", "sleep 2"])
        .env_remove("OCI_TOOLS_LOG")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("failed to spawn ocirun exec --detach");
    assert!(exec.success(), "ocirun exec --detach failed: {exec:?}");

    // Written already, by the time the (fast, detached) call above
    // returned -- no poll-and-wait needed, unlike the non-detached
    // `--pid-file` test above, which spawns `ocirun exec` itself in
    // the background and polls because *that* call blocks for the
    // full 30s sleep.
    assert!(pid_file.exists(), "--pid-file was never written");
    let file_content = std::fs::read_to_string(&pid_file).unwrap();
    let exec_pid: i32 = file_content.trim().parse().unwrap_or_else(|e| {
        panic!("--pid-file content {file_content:?} not a plain decimal pid: {e}")
    });
    assert!(exec_pid > 0);

    ocirun(
        root_dir.path(),
        &["delete", "--force", "exec-detach-pidfile-test"],
    );
}

/// A real, checked-directly divergence from a non-detached `exec`:
/// `--detach` always exits `0`, regardless of whatever the detached
/// command will *eventually* exit with -- matching real `runc exec
/// --detach`/`crun exec --detach`'s own identical unconditional
/// `return 0` exactly (this project's own success just means "the
/// exec'd process started", not "and it later succeeded too").
#[test]
fn exec_detach_exits_zero_even_though_the_detached_command_will_eventually_fail() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "sleep 30"]);

    let create = ocirun_create(root_dir.path(), bundle_dir.path(), "exec-detach-fail-test");
    assert!(create.status.success(), "{create:?}");
    let start = ocirun(root_dir.path(), &["start", "exec-detach-fail-test"]);
    assert!(start.status.success());
    assert_eq!(
        wait_for_status(
            root_dir.path(),
            "exec-detach-fail-test",
            "running",
            Duration::from_secs(5)
        ),
        "running"
    );

    // `Stdio::null()` for the same real reason the other two tests
    // above document -- even though this particular detached command
    // exits almost instantly either way, matching the same safe
    // pattern consistently rather than relying on that coincidence.
    let exec = std::process::Command::new(bin_path("ocirun"))
        .arg("--root")
        .arg(root_dir.path())
        .args([
            "exec",
            "--detach",
            "exec-detach-fail-test",
            "/bin/sh",
            "-c",
            "exit 7",
        ])
        .env_remove("OCI_TOOLS_LOG")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("failed to spawn ocirun exec --detach");
    assert!(
        exec.success(),
        "--detach should exit 0 regardless of the detached command's own eventual exit code: \
         {exec:?}"
    );

    ocirun(
        root_dir.path(),
        &["delete", "--force", "exec-detach-fail-test"],
    );
}

/// `exec --process-label`/`--apparmor` (`docs/design/0562`, matching
/// real `runc exec --process-label`/`--apparmor`/`crun exec
/// --process-label`/`--apparmor` exactly) are real, previously-
/// unrecognized flags -- given with a real, non-empty value, this
/// project's own honest lack of any SELinux/AppArmor support at all
/// is a clear, immediate error rather than silently pretending to
/// apply either. Given with an empty value (or not given at all),
/// both are a true no-op -- exec still runs normally.
#[test]
fn exec_process_label_and_apparmor_reject_a_real_value_but_accept_an_empty_one() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "sleep 30"]);

    let create = ocirun_create(root_dir.path(), bundle_dir.path(), "exec-lsm-test");
    assert!(create.status.success(), "{create:?}");
    let start = ocirun(root_dir.path(), &["start", "exec-lsm-test"]);
    assert!(start.status.success());
    assert_eq!(
        wait_for_status(
            root_dir.path(),
            "exec-lsm-test",
            "running",
            Duration::from_secs(5)
        ),
        "running"
    );

    let apparmor_real_value = ocirun(
        root_dir.path(),
        &[
            "exec",
            "--apparmor",
            "some-profile",
            "exec-lsm-test",
            "/bin/true",
        ],
    );
    assert!(!apparmor_real_value.status.success());
    assert!(
        String::from_utf8_lossy(&apparmor_real_value.stderr).contains("not yet supported"),
        "{apparmor_real_value:?}"
    );

    let process_label_real_value = ocirun(
        root_dir.path(),
        &[
            "exec",
            "--process-label",
            "system_u:object_r:some_t:s0",
            "exec-lsm-test",
            "/bin/true",
        ],
    );
    assert!(!process_label_real_value.status.success());
    assert!(
        String::from_utf8_lossy(&process_label_real_value.stderr).contains("not yet supported"),
        "{process_label_real_value:?}"
    );

    let apparmor_empty = ocirun(
        root_dir.path(),
        &["exec", "--apparmor", "", "exec-lsm-test", "/bin/true"],
    );
    assert!(
        apparmor_empty.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&apparmor_empty.stderr)
    );

    let process_label_empty = ocirun(
        root_dir.path(),
        &["exec", "--process-label", "", "exec-lsm-test", "/bin/true"],
    );
    assert!(
        process_label_empty.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&process_label_empty.stderr)
    );

    let unaffected = ocirun(root_dir.path(), &["exec", "exec-lsm-test", "/bin/true"]);
    assert!(unaffected.status.success(), "{unaffected:?}");

    ocirun(root_dir.path(), &["delete", "--force", "exec-lsm-test"]);
}
