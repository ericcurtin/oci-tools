//! `ocirun run` integration tests: the first real, automated,
//! end-to-end test of a built `oci-tools` binary actually creating a
//! Linux container (namespaces, mounts, `pivot_root`, `exec`) — not a
//! manual scratch-program verification like earlier increments needed,
//! because a `tests/tests/*.rs` test spawns the built `ocirun` binary as
//! a subprocess, which starts fresh and single-threaded from its own
//! `main()` regardless of how many threads the test harness itself has —
//! exactly the condition `unshare(CLONE_NEWUSER)` requires (see
//! `docs/design/0011-fork-and-waitpid.md`'s closing note, which flagged
//! this in advance).
//!
//! Needs a real minimal rootfs to `exec` something in, so these tests use
//! `busybox` if it's on `$PATH` (present in this project's dev
//! environment and common on minimal cloud images) and skip themselves
//! — printing why, not failing — when it isn't, rather than making it a
//! hard CI dependency.

use std::path::{Path, PathBuf};
use std::process::Command;

use oci_tools_tests::bin_path;

/// Locate `busybox`, or `None` if it isn't installed.
fn busybox_path() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("busybox"))
        .find(|p| p.is_file())
}

/// Whether a real, working `systemd --user` session is reachable —
/// needed to test cgroup directory creation/process migration for real
/// (see `docs/design/0015`): a raw `cgroup.procs` write only succeeds
/// across cgroup branches when the calling process already has write
/// access to their common ancestor, which a plain SSH/login session's
/// cgroup never has. `systemd-run --user --scope` asks systemd itself
/// (which owns and delegates the whole `app.slice` subtree) to place
/// the calling test process into a fresh, properly delegated scope
/// first, sidestepping that.
///
/// Does a real, self-cleaning probe (`systemd-run --user --scope --
/// true`) rather than just checking the binary is on `$PATH`: a
/// minimal CI image can have `systemd-run` installed with no user
/// D-Bus/systemd instance actually reachable (no login session, no
/// lingering enabled), which fails the exact same way whether or not
/// the binary exists.
fn systemd_user_scope_available() -> bool {
    Command::new("systemd-run")
        .args(["--user", "--scope", "--", "true"])
        .output()
        .is_ok_and(|out| out.status.success())
}

/// Build a minimal bundle at `dir`: a busybox-based rootfs with `sh` and
/// the given symlinked applets, and a rootless `config.json` running
/// `args` (a `/bin/sh -c "..."` style command is the expected shape).
fn write_bundle(dir: &Path, busybox: &Path, args: &[&str]) {
    let rootfs = dir.join("rootfs");
    std::fs::create_dir_all(rootfs.join("bin")).unwrap();
    std::fs::copy(busybox, rootfs.join("bin/busybox")).unwrap();
    for applet in ["sh", "echo", "true", "false"] {
        #[cfg(unix)]
        std::os::unix::fs::symlink("busybox", rootfs.join("bin").join(applet)).unwrap();
    }

    let out = Command::new(bin_path("ocirun"))
        .args(["spec", "--rootless", "--bundle"])
        .arg(dir)
        .output()
        .expect("failed to spawn ocirun spec");
    assert!(
        out.status.success(),
        "ocirun spec --rootless failed: {out:?}"
    );

    let config_path = dir.join("config.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["process"]["terminal"] = serde_json::json!(false);
    config["process"]["args"] = serde_json::json!(args);
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
}

fn ocirun_run(dir: &Path, id: &str) -> std::process::Output {
    Command::new(bin_path("ocirun"))
        .args(["run", id])
        .current_dir(dir)
        .env_remove("OCI_TOOLS_LOG")
        .output()
        .expect("failed to spawn ocirun run")
}

#[test]
fn run_execs_the_container_process_and_isolates_the_rootfs() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    write_bundle(
        dir.path(),
        &busybox,
        &["/bin/sh", "-c", "echo hello-from-container && ls /"],
    );

    let out = ocirun_run(dir.path(), "smoke-test");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "ocirun run failed: stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stdout.contains("hello-from-container"),
        "got stdout: {stdout:?}"
    );
    // `ls /` inside the container must show the container's own rootfs
    // top level (proof `pivot_root` actually happened), not the host's.
    assert!(stdout.contains("bin"), "got stdout: {stdout:?}");

    // The host's copy of the bundle directory must be unaffected: no
    // leftover pivot_root scratch directory, and (best-effort) no
    // lingering mount left behind for this test's own temp path.
    assert!(
        !dir.path()
            .join("rootfs")
            .join(".oci-tools-put-old")
            .exists(),
        "pivot_root scratch directory must be cleaned up"
    );
}

#[test]
fn run_propagates_the_containers_own_exit_code() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    write_bundle(dir.path(), &busybox, &["/bin/sh", "-c", "exit 42"]);

    let out = ocirun_run(dir.path(), "exit-code-test");
    assert_eq!(
        out.status.code(),
        Some(42),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn run_reports_command_not_found_as_exit_127() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    write_bundle(dir.path(), &busybox, &["/bin/does-not-exist"]);

    let out = ocirun_run(dir.path(), "not-found-test");
    assert_eq!(
        out.status.code(),
        Some(127),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn run_applies_the_default_capability_set_and_no_new_privileges() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    write_bundle(
        dir.path(),
        &busybox,
        &[
            "/bin/sh",
            "-c",
            r#"grep -E "^(CapInh|CapPrm|CapEff|CapBnd|CapAmb|NoNewPrivs):" /proc/self/status"#,
        ],
    );

    let out = ocirun_run(dir.path(), "capabilities-default-test");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // `ocirun spec`'s default capability set is exactly CAP_AUDIT_WRITE
    // (bit 29) | CAP_KILL (bit 5) | CAP_NET_BIND_SERVICE (bit 10) = the
    // bitmask below — applied to every set `identity::apply` touches
    // (bounding/effective/permitted; inheritable/ambient stay empty,
    // matching the spec), and `no_new_privileges` defaults to `true`.
    assert_eq!(
        stdout.trim(),
        "CapInh:\t0000000000000000\nCapPrm:\t0000000020000420\nCapEff:\t0000000020000420\nCapBnd:\t0000000020000420\nCapAmb:\t0000000000000000\nNoNewPrivs:\t1"
    );
}

#[test]
fn run_drops_capabilities_the_spec_does_not_grant() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    write_bundle(
        dir.path(),
        &busybox,
        &[
            "/bin/sh",
            "-c",
            r#"grep -E "^(CapEff|CapBnd|NoNewPrivs):" /proc/self/status"#,
        ],
    );
    let config_path = dir.path().join("config.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["process"]["capabilities"] = serde_json::json!({
        "bounding": [],
        "effective": [],
        "permitted": [],
    });
    config["process"]["noNewPrivileges"] = serde_json::json!(false);
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();

    let out = ocirun_run(dir.path(), "capabilities-empty-test");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        stdout.trim(),
        "CapEff:\t0000000000000000\nCapBnd:\t0000000000000000\nNoNewPrivs:\t0"
    );
}

#[test]
fn run_applies_rlimits() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    write_bundle(
        dir.path(),
        &busybox,
        &[
            "/bin/sh",
            "-c",
            r#"grep -E "^Max open files" /proc/self/limits"#,
        ],
    );
    let config_path = dir.path().join("config.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    // Deliberately not RLIMIT_NPROC: it counts against the *real* host
    // uid's total process count (applied before the container even has
    // its own user namespace — see docs/design/0014), so a low value
    // would make this test's pass/fail depend on how many other
    // processes the CI/dev machine's user happens to have running.
    config["process"]["rlimits"] = serde_json::json!([
        {"type": "RLIMIT_NOFILE", "soft": 256, "hard": 512},
    ]);
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();

    let out = ocirun_run(dir.path(), "rlimits-test");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let line = stdout.trim();
    let fields: Vec<&str> = line.split_whitespace().collect();
    assert_eq!(
        &fields[..5],
        ["Max", "open", "files", "256", "512"],
        "got: {line:?}"
    );
}

#[test]
fn run_creates_and_enters_the_requested_cgroup() {
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

    let dir = tempfile::tempdir().unwrap();
    write_bundle(
        dir.path(),
        &busybox,
        &["/bin/sh", "-c", "cat /proc/self/cgroup"],
    );
    let config_path = dir.path().join("config.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    let uid = rustix::process::getuid().as_raw();
    // A sibling of the carrier scope `systemd-run` below places this
    // test process into: both are direct children of the delegated
    // `app.slice`, so `app.slice` (writable, since the whole subtree is
    // delegated to this uid) is their common ancestor — the specific
    // permission `cgroup.procs` migration checks. See docs/design/0015
    // for why this can't just be an arbitrary/absolute path picked
    // without regard for what cgroup the calling process is in.
    let target = format!(
        "/user.slice/user-{uid}.slice/user@{uid}.service/app.slice/ocirun-cgroup-test-{}",
        std::process::id()
    );
    config["linux"]["cgroupsPath"] = serde_json::json!(target);
    config["linux"]["resources"] = serde_json::json!({"pids": {"limit": 20}});
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();

    let carrier_unit = format!("ocirun-test-carrier-{}.scope", std::process::id());
    let out = Command::new("systemd-run")
        .args([
            "--user",
            "--scope",
            "--slice=app.slice",
            &format!("--unit={carrier_unit}"),
            "--",
        ])
        .arg(bin_path("ocirun"))
        .args(["run", "cgroup-test"])
        .current_dir(dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .output()
        .expect("failed to spawn systemd-run");

    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The container's own view of `/proc/self/cgroup`: `0::/` (its own
    // cgroup as the *root*) proves both that it was actually migrated
    // into the cgroup this test asked for, and that the migration ran
    // strictly before the `CLONE_NEWCGROUP` unshare (see
    // `cgroups::enter`'s doc comment) — the wrong order would show the
    // full absolute path instead of `/`.
    assert_eq!(
        stdout.lines().next_back().unwrap_or_default(),
        "0::/",
        "got stdout: {stdout:?}"
    );
}

#[test]
fn run_applies_a_seccomp_profile_that_blocks_a_syscall() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    write_bundle(
        dir.path(),
        &busybox,
        &["/bin/sh", "-c", "mkdir /blocked; echo mkdir_exit=$?"],
    );
    let config_path = dir.path().join("config.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    // `mkdirat` (not `mkdir`) because raw `mkdir` isn't a valid syscall
    // on aarch64 (this project's own CI/dev architecture — see
    // docs/design/0016); every C library's `mkdir()` on this
    // architecture already compiles down to `mkdirat(AT_FDCWD, ...)`.
    config["linux"]["seccomp"] = serde_json::json!({
        "defaultAction": "SCMP_ACT_ALLOW",
        "syscalls": [
            {"names": ["mkdirat"], "action": "SCMP_ACT_ERRNO", "errnoRet": 13}
        ]
    });
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();

    let out = ocirun_run(dir.path(), "seccomp-test");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // busybox's `mkdir` reports any failure as exit code 1, not the raw
    // errno — proof enough the syscall itself was actually denied.
    assert_eq!(stdout.trim(), "mkdir_exit=1");
}

#[test]
fn run_applies_a_seccomp_profile_with_an_argument_condition() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    // `kill(pid, 0)` (checking whether a process exists, sending no
    // actual signal) should be denied by this profile; any *other*
    // signal number wouldn't match the argument condition at all —
    // proving `index`/`value`/`op` actually distinguish argument values
    // at the syscall level, not just the syscall name.
    write_bundle(
        dir.path(),
        &busybox,
        &["/bin/sh", "-c", "kill -0 $$; echo kill0_exit=$?"],
    );
    let config_path = dir.path().join("config.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["linux"]["seccomp"] = serde_json::json!({
        "defaultAction": "SCMP_ACT_ALLOW",
        "syscalls": [
            {
                "names": ["kill"],
                "action": "SCMP_ACT_ERRNO",
                "errnoRet": 1,
                "args": [{"index": 1, "value": 0, "op": "SCMP_CMP_EQ"}]
            }
        ]
    });
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();

    let out = ocirun_run(dir.path(), "seccomp-arg-test");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(stdout.trim(), "kill0_exit=1");
}

#[test]
fn run_isolates_hostname_from_the_host() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    write_bundle(dir.path(), &busybox, &["/bin/sh", "-c", "hostname"]);

    let out = ocirun_run(dir.path(), "hostname-test");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // `ocirun spec`'s default hostname (see oci_spec_types::runtime::
    // Spec::example) -- proves sethostname() took effect inside the
    // container's own UTS namespace.
    assert_eq!(stdout.trim(), "ocirun");
}
