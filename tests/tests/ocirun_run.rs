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

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use oci_tools_tests::{bin_path, busybox_path, ocirun, state_status, write_bundle};

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

fn ocirun_run(dir: &Path, id: &str) -> std::process::Output {
    // `--root`, a sibling directory of the bundle itself rather than
    // this project's own real, shared default (`docs/design/0373`
    // gave `ocirun run` a real, tracked state record for the first
    // time) -- matching every other test file's own already-
    // established "always an isolated, per-test root" convention,
    // computed here rather than threaded through every one of this
    // file's own call sites, none of which need to know it exists at
    // all.
    Command::new(bin_path("ocirun"))
        .args(["run", id])
        .current_dir(dir)
        .args(["--root"])
        .arg(dir.join("state-root"))
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

/// A bundle whose `config.json` declares no `process.user.umask` at
/// all gets the same real, deterministic `0022` default every real
/// `runc`/`crun` themselves always fall back to
/// (`oci_runtime_core::identity::apply`'s own fallback,
/// `~/git/crun/src/libcrun/container.c:1447`) -- a real,
/// previously-silent gap: this project's containers used to simply
/// inherit whatever umask their own *launching* process happened to
/// have, never calling `umask(2)` at all anywhere.
#[test]
fn run_defaults_to_umask_0022_when_the_bundle_declares_none() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    write_bundle(dir.path(), &busybox, &["/bin/sh", "-c", "umask"]);

    let out = ocirun_run(dir.path(), "umask-default-test");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "0022");
}

/// An explicit `process.user.umask` in the bundle's own `config.json`
/// is genuinely applied, matching real `runc`/`crun` exactly (neither
/// has a CLI flag of its own for this -- it's a pure `config.json`
/// field, set by whoever generates the bundle, e.g. `ociman run
/// --umask`).
#[test]
fn run_honors_an_explicit_umask_declared_in_the_bundle() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    write_bundle(dir.path(), &busybox, &["/bin/sh", "-c", "umask"]);
    let config_path = dir.path().join("config.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["process"]["user"]["umask"] = serde_json::json!(0o77);
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();

    let out = ocirun_run(dir.path(), "umask-explicit-test");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "0077");
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

    // The cgroup directory itself must be gone once `run` returns —
    // the kernel does not remove an empty cgroup on its own (see
    // `docs/design/0027`); leaving this unchecked would silently leak
    // one directory per container run with a `cgroupsPath` forever.
    let cgroup_dir = Path::new("/sys/fs/cgroup").join(target.trim_start_matches('/'));
    assert!(
        !cgroup_dir.exists(),
        "cgroup directory {} should have been removed after run",
        cgroup_dir.display()
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
    // The real syscall `mkdir(1)` uses is genuinely architecture-
    // dependent, not just `mkdirat` everywhere: glibc's own `mkdir()`
    // (`sysdeps/unix/sysv/linux/mkdir.c`) calls the legacy `mkdir`
    // syscall directly whenever the target still has one at all
    // (`#ifdef __NR_mkdir`) — true on x86_64, which keeps it for
    // compatibility — and only falls back to `mkdirat(AT_FDCWD, ...)`
    // on architectures that never had a standalone `mkdir` syscall to
    // begin with, aarch64 among them (see `docs/design/0016`, this
    // project's own CI/dev architecture at the time this test was
    // first written — the only one this had ever actually been
    // verified against until real x86_64 CI hardware finally reached
    // this test and found naming only `mkdirat` blocks nothing there
    // at all). Picks the one real name that's actually correct for the
    // current build's own target_arch, rather than unioning both
    // unconditionally — a strict profile like this one genuinely
    // rejects an unresolvable name on the wrong architecture (`mkdir`
    // doesn't exist as a syscall on aarch64 at all).
    let mkdir_syscall_name = if cfg!(target_arch = "x86_64") {
        "mkdir"
    } else {
        "mkdirat"
    };
    config["linux"]["seccomp"] = serde_json::json!({
        "defaultAction": "SCMP_ACT_ALLOW",
        "syscalls": [
            {"names": [mkdir_syscall_name], "action": "SCMP_ACT_ERRNO", "errnoRet": 13}
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
fn run_applies_a_seccomp_profile_with_two_distinct_non_default_actions() {
    // Real, multi-action profiles (a different action per `syscalls[]`
    // entry, not just one shared action plus a default) were rejected
    // outright before `docs/design/0036` -- see that note for why, and
    // for the manual-scratch/real-captured-profile verification this
    // automated test doesn't repeat (proving the *specific* "an
    // explicit ALLOW overrides a stricter ERRNO default" case, which
    // this test's own `defaultAction: SCMP_ACT_ALLOW` choice
    // deliberately doesn't need, to stay reliably portable rather than
    // needing an exhaustive allow-list for every syscall this
    // rootless container's own busybox shell happens to make).
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
            "mkdir /blocked; echo mkdir_exit=$?; kill -0 $$; echo kill0_exit=$?",
        ],
    );
    let config_path = dir.path().join("config.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    // Same real per-architecture `mkdir` syscall choice as
    // `run_applies_a_seccomp_profile_that_blocks_a_syscall`'s own doc
    // comment explains in full.
    let mkdir_syscall_name = if cfg!(target_arch = "x86_64") {
        "mkdir"
    } else {
        "mkdirat"
    };
    config["linux"]["seccomp"] = serde_json::json!({
        "defaultAction": "SCMP_ACT_ALLOW",
        "syscalls": [
            {"names": [mkdir_syscall_name], "action": "SCMP_ACT_ERRNO", "errnoRet": 13},
            {
                "names": ["kill"],
                "action": "SCMP_ACT_ERRNO",
                "errnoRet": 1,
                "args": [{"index": 1, "value": 0, "op": "SCMP_CMP_EQ"}]
            }
        ]
    });
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();

    let out = ocirun_run(dir.path(), "seccomp-multi-action-test");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Both distinct actions took effect on the *same* container, from
    // the *same* profile -- exactly what was rejected before this
    // increment.
    assert_eq!(stdout.trim(), "mkdir_exit=1\nkill0_exit=1");
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

/// `ocirun run --preserve-fds` (0291), matching real `runc run`/`crun
/// run --preserve-fds` exactly: by default, every fd above stdio is
/// closed before the container's own process ever runs -- a real,
/// previously-missing step this project's own launch sequence never
/// performed at all before this (any fd this process's own caller
/// happened to have open beyond stdio would have leaked straight into
/// the container, unconditionally). `--preserve-fds N` keeps exactly
/// the first `N` of them (starting at fd 3, right after stdio)
/// instead.
///
/// Uses a real `pre_exec` closure on the *test's own* spawned
/// `ocirun run` subprocess to `dup2` an already-open, real file onto
/// exactly fd 3 in that one child only (never touching this test
/// process's own fd 3) -- the same "starts right after stdio" slot
/// real `--preserve-fds` semantics assume, needed to make the test
/// deterministic regardless of whatever fd numbers the test harness
/// itself already happens to have open.
#[test]
fn run_preserve_fds_closes_extra_fds_by_default_but_keeps_them_with_the_flag() {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::process::CommandExt as _;

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
            "test -e /proc/self/fd/3 && echo fd3-present || echo fd3-absent",
        ],
    );

    let run_with_fd3_open = |extra_args: &[&str]| -> std::process::Output {
        let marker = tempfile::NamedTempFile::new().unwrap();
        let raw_fd = marker.as_file().as_raw_fd();
        let mut cmd = Command::new(bin_path("ocirun"));
        cmd.arg("run")
            .args(extra_args)
            .arg("preserve-fds-test")
            .current_dir(dir.path())
            .env_remove("OCI_TOOLS_LOG");
        // SAFETY: only calls `dup2(2)`/`fcntl(2)` (both async-signal-
        // safe, no allocation) in the forked-but-not-yet-exec'd child,
        // only ever affecting that child's own fd table.
        #[allow(unsafe_code)]
        unsafe {
            cmd.pre_exec(move || {
                if libc::dup2(raw_fd, 3) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // A real, easy-to-hit POSIX gotcha, found by hand: if
                // `raw_fd` already happens to equal `3` in this
                // process (a real possibility -- fd numbers depend on
                // whatever this test binary's own harness already has
                // open), `dup2(3, 3)` is specified to be a genuine
                // no-op that leaves fd 3's own `FD_CLOEXEC` flag
                // untouched (unlike an ordinary `dup2` to a
                // *different* target fd, which always clears it on
                // the new descriptor) -- so if the tempfile crate
                // happened to open its own fd with `O_CLOEXEC` (a
                // sensible default), fd 3 would still get silently
                // closed by the kernel at the real `execve()` just
                // below, with no error at all, indistinguishable from
                // `--preserve-fds` genuinely not working. Clearing
                // `FD_CLOEXEC` explicitly here, unconditionally,
                // closes that gap regardless of which case applies.
                if libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let out = cmd.output().expect("failed to spawn ocirun run");
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
}

/// `--preserve-fds N` fails fast, before ever forking a container at
/// all, if fewer than `N` fds are actually open starting at fd 3 --
/// matching real runc's own identical upfront `Faccessat` check
/// exactly (see `verify_preserve_fds`'s own doc comment in
/// `bin/ocirun/src/main.rs`).
#[test]
fn run_preserve_fds_rejects_a_claim_with_no_matching_open_fd() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    write_bundle(dir.path(), &busybox, &["/bin/sh", "-c", "true"]);

    let out = Command::new(bin_path("ocirun"))
        .args(["run", "--preserve-fds", "5", "preserve-fds-reject-test"])
        .current_dir(dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .output()
        .expect("failed to spawn ocirun run");
    assert!(!out.status.success(), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("is not open in this process"),
        "{out:?}"
    );
}

/// `ocirun run --no-pivot` (matching real `runc run`/`crun run
/// --no-pivot` exactly): a `chroot`-style root swap instead of
/// `pivot_root(2)` still genuinely isolates the container's own
/// rootfs (`ls /` shows only the container's own top level, not the
/// host's), producing the exact same real, user-visible result as the
/// default `pivot_root` path — the two are meant to be
/// indistinguishable from inside the container, only the low-level
/// mechanism differs.
#[test]
fn run_no_pivot_still_isolates_the_rootfs_just_like_pivot_root() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    write_bundle(
        dir.path(),
        &busybox,
        &["/bin/sh", "-c", "echo hello-no-pivot && ls /"],
    );

    let out = Command::new(bin_path("ocirun"))
        .args(["run", "--no-pivot", "no-pivot-test"])
        .current_dir(dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .output()
        .expect("failed to spawn ocirun run --no-pivot");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "ocirun run --no-pivot failed: stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(stdout.contains("hello-no-pivot"), "got stdout: {stdout:?}");
    // Proof the chroot-style swap actually isolated the rootfs, same
    // as `run_execs_the_container_process_and_isolates_the_rootfs`'s
    // own identical check for the default `pivot_root` path.
    assert!(stdout.contains("bin"), "got stdout: {stdout:?}");

    // No `RootfsAction::UnmountOldRoot`-style leftover directory at
    // all on this path (there is no relocated old root in the first
    // place, unlike real `pivot_root`) -- confirms the plan's own
    // `UnmountOldRoot` step was genuinely skipped, not silently
    // executed against a chroot'd tree it was never meant to run
    // against.
    assert!(
        !dir.path()
            .join("rootfs")
            .join(".oci-tools-put-old")
            .exists(),
        "no pivot_root scratch directory should exist at all on the --no-pivot path"
    );
}

/// Like `oci_tools_tests::wait_for_status`, but tolerant of the state
/// record not existing *at all* yet -- unlike every one of that
/// shared helper's own existing call sites (always polled only after
/// an `ocirun create`/`start` invocation has already returned
/// successfully, guaranteeing a record already exists), this file's
/// own new tests below start polling the instant a freshly `spawn()`ed
/// `ocirun run` child process is launched, racing against its own
/// `store.create` call genuinely running at all yet.
fn wait_for_status_tolerating_not_yet_created(
    root: &Path,
    id: &str,
    want: &str,
    timeout: Duration,
) -> String {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let out = Command::new(bin_path("ocirun"))
            .args(["--root"])
            .arg(root)
            .args(["state", id])
            .env_remove("OCI_TOOLS_LOG")
            .output()
            .expect("failed to spawn ocirun state");
        if out.status.success() {
            let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
            let status = json["status"].as_str().unwrap().to_string();
            if status == want || std::time::Instant::now() >= deadline {
                return status;
            }
        } else if std::time::Instant::now() >= deadline {
            return "does-not-exist".to_string();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// `ocirun run` (`docs/design/0373`): a real, tracked state record for
/// the container's own entire lifetime, matching real `runc run`'s
/// own checked-directly behavior exactly (`~/git/runc/utils_linux.go`'s
/// own `startContainer`: `run` and `create` both call the identical,
/// state-persisting factory call internally). A concurrent `ocirun
/// state`/`list` call, issued from an entirely separate invocation
/// while the original `ocirun run` is still blocked in the
/// foreground, now sees the real, running container -- and, once
/// that foreground `ocirun run` actually exits, the state is
/// completely removed again (real runc's own checked-directly default
/// with no `--keep` given), leaving nothing behind for `ocirun state`
/// to find afterward.
#[test]
fn run_is_visible_to_a_concurrent_state_query_then_fully_removed_after_exit() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "sleep 30"]);

    let child = Command::new(bin_path("ocirun"))
        .args(["--root"])
        .arg(root_dir.path())
        .args(["run", "run-visibility-test", "--bundle"])
        .arg(bundle_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn ocirun run");

    // The concurrent, entirely separate `ocirun state`/`list`
    // invocations below are the real point of this test -- they must
    // see the exact same container the still-blocked `child` above is
    // running, not nothing at all (the previous, untracked behavior).
    assert_eq!(
        wait_for_status_tolerating_not_yet_created(
            root_dir.path(),
            "run-visibility-test",
            "running",
            Duration::from_secs(5)
        ),
        "running",
        "a concurrent `ocirun state` should see the container reach running"
    );
    let list = ocirun(root_dir.path(), &["list", "--format", "json"]);
    assert!(list.status.success(), "{list:?}");
    let entries: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    let ids: Vec<&str> = entries
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["id"].as_str().unwrap())
        .collect();
    assert!(
        ids.contains(&"run-visibility-test"),
        "a concurrent `ocirun list` should see it too: {ids:?}"
    );

    // End it early rather than waiting out the full `sleep 30`.
    let kill = ocirun(root_dir.path(), &["kill", "run-visibility-test", "KILL"]);
    assert!(kill.status.success(), "{kill:?}");

    let output = child
        .wait_with_output()
        .expect("failed to wait for ocirun run");
    assert!(
        !output.status.success(),
        "a KILL-terminated container reports a real, nonzero (128+signal) exit code, matching \
         run_reports_command_not_found_as_exit_127's own identical convention: {output:?}"
    );

    // Real runc's own checked-directly default (no `--keep`): nothing
    // left behind at all once the foreground `run` is actually done.
    let state = Command::new(bin_path("ocirun"))
        .args(["--root"])
        .arg(root_dir.path())
        .args(["state", "run-visibility-test"])
        .env_remove("OCI_TOOLS_LOG")
        .output()
        .expect("failed to spawn ocirun state");
    assert!(
        !state.status.success(),
        "the container's own state should be completely gone after `run` exits: {state:?}"
    );
}

/// A second `ocirun run` reusing an id that's still genuinely in use
/// (another, still-blocked `ocirun run` of the same id) is a real,
/// clear error -- matching real `runc run`/`runc create`'s own
/// identical "container with given ID already exists" refusal exactly
/// (container IDs are unique within one state root, the same rule
/// `ocirun create` already enforces).
#[test]
fn run_of_an_id_already_in_use_is_a_clear_error() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "sleep 30"]);

    let mut first = Command::new(bin_path("ocirun"))
        .args(["--root"])
        .arg(root_dir.path())
        .args(["run", "run-duplicate-id-test", "--bundle"])
        .arg(bundle_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn the first ocirun run");
    assert_eq!(
        wait_for_status_tolerating_not_yet_created(
            root_dir.path(),
            "run-duplicate-id-test",
            "running",
            Duration::from_secs(5)
        ),
        "running"
    );

    let second = Command::new(bin_path("ocirun"))
        .args(["--root"])
        .arg(root_dir.path())
        .args(["run", "run-duplicate-id-test", "--bundle"])
        .arg(bundle_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .output()
        .expect("failed to spawn the second ocirun run");
    assert!(!second.status.success());
    assert_eq!(
        state_status(root_dir.path(), "run-duplicate-id-test"),
        "running",
        "the first, still-running container must be completely unaffected by the second, \
         rejected attempt"
    );

    let kill = ocirun(root_dir.path(), &["kill", "run-duplicate-id-test", "KILL"]);
    assert!(kill.status.success(), "{kill:?}");
    let _ = first.wait();
}

/// `ocirun run --keep` (`docs/design/0373`) leaves the container's own
/// state queryable afterward, matching real `runc run --keep`/`crun
/// run --keep` exactly (checked directly,
/// `~/git/runc/utils_linux.go`'s own `shouldDestroy: !cmd.Bool
/// ("keep")`) — a later `ocirun delete` is still needed to actually
/// clean it up, matching real runc's own identical two-step
/// expectation.
#[test]
fn run_keep_leaves_a_real_stopped_state_behind_for_a_later_delete() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "exit 7"]);

    let out = Command::new(bin_path("ocirun"))
        .args(["--root"])
        .arg(root_dir.path())
        .args(["run", "run-keep-test", "--bundle"])
        .arg(bundle_dir.path())
        .args(["--keep"])
        .env_remove("OCI_TOOLS_LOG")
        .output()
        .expect("failed to spawn ocirun run --keep");
    assert_eq!(out.status.code(), Some(7), "{out:?}");

    assert_eq!(
        state_status(root_dir.path(), "run-keep-test"),
        "stopped",
        "the container's own state must still be queryable after a --keep run"
    );

    let delete = ocirun(root_dir.path(), &["delete", "run-keep-test"]);
    assert!(delete.status.success(), "{delete:?}");
    let state = Command::new(bin_path("ocirun"))
        .args(["--root"])
        .arg(root_dir.path())
        .args(["state", "run-keep-test"])
        .env_remove("OCI_TOOLS_LOG")
        .output()
        .expect("failed to spawn ocirun state");
    assert!(
        !state.status.success(),
        "a later `ocirun delete` should still be needed, and actually work: {state:?}"
    );
}

/// Without `--keep` (the default), nothing is left behind at all —
/// same real assertion `run_is_visible_to_a_concurrent_state_query_
/// then_fully_removed_after_exit` already makes, kept here too as a
/// direct, explicit contrast right next to the `--keep` test above.
#[test]
fn run_without_keep_removes_the_state_entirely() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "exit 0"]);

    let out = ocirun_run(bundle_dir.path(), "run-no-keep-test");
    assert!(out.status.success(), "{out:?}");

    let state = Command::new(bin_path("ocirun"))
        .args(["--root"])
        .arg(bundle_dir.path().join("state-root"))
        .args(["state", "run-no-keep-test"])
        .env_remove("OCI_TOOLS_LOG")
        .output()
        .expect("failed to spawn ocirun state");
    assert!(!state.status.success(), "{state:?}");
    let _ = root_dir;
}

/// `ocirun run --detach`/`-d` (real runc's/crun's own detach flag,
/// mirroring `ociman run -d`'s own already-shipped keeper-process
/// pattern, `docs/design/0098`) returns almost immediately -- long
/// before a `sleep 30`'d container's own real command finishes -- and
/// leaves it genuinely running, queryable via a concurrent `ocirun
/// state`, exactly as if this had been a foreground `run` a separate
/// invocation happened to observe mid-flight. Also confirms real
/// runc's own checked-directly silence on success (unlike `ociman run
/// -d`'s own id-printing convention): nothing at all is printed to
/// stdout.
#[test]
fn run_detach_returns_immediately_with_the_container_still_running() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "sleep 30"]);

    let started = std::time::Instant::now();
    let out = Command::new(bin_path("ocirun"))
        .args(["--root"])
        .arg(root_dir.path())
        .args(["run", "--detach", "run-detach-test", "--bundle"])
        .arg(bundle_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .output()
        .expect("failed to spawn ocirun run --detach");
    assert!(out.status.success(), "{out:?}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "a detached run must return long before the container's own \
         30-second sleep finishes, took {:?}",
        started.elapsed()
    );
    assert!(
        out.stdout.is_empty(),
        "real `runc run -d` prints nothing at all on success, unlike `ociman run -d`'s own \
         id-printing convention: {out:?}"
    );

    assert_eq!(
        state_status(root_dir.path(), "run-detach-test"),
        "running",
        "the detached container must already be genuinely running by the time the \
         original invocation returns"
    );

    let kill = ocirun(root_dir.path(), &["kill", "run-detach-test", "KILL"]);
    assert!(kill.status.success(), "{kill:?}");
    assert_eq!(
        wait_for_status_tolerating_not_yet_created(
            root_dir.path(),
            "run-detach-test",
            "does-not-exist",
            Duration::from_secs(5),
        ),
        "does-not-exist",
        "without --keep, the detached container's own state must be removed once it exits, \
         same as a foreground run"
    );
}

/// `ocirun run --detach --keep` combined: the container's own state
/// survives after it exits (queryable, `stopped`), exactly like a
/// foreground `--keep`'d run, just reached asynchronously instead.
#[test]
fn run_detach_keep_leaves_a_stopped_state_behind_for_a_later_delete() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "exit 9"]);

    let out = Command::new(bin_path("ocirun"))
        .args(["--root"])
        .arg(root_dir.path())
        .args([
            "run",
            "--detach",
            "--keep",
            "run-detach-keep-test",
            "--bundle",
        ])
        .arg(bundle_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .output()
        .expect("failed to spawn ocirun run --detach --keep");
    assert!(out.status.success(), "{out:?}");

    // The container's own command (`exit 9`) may not have actually
    // finished by the moment the detaching invocation above returns
    // (it only waits for `Creating` to clear, not for real exit) --
    // poll for `stopped` rather than asserting it immediately.
    assert_eq!(
        wait_for_status_tolerating_not_yet_created(
            root_dir.path(),
            "run-detach-keep-test",
            "stopped",
            Duration::from_secs(5),
        ),
        "stopped",
        "a --keep'd detached container's own state must still be queryable once it exits"
    );

    let delete = ocirun(root_dir.path(), &["delete", "run-detach-keep-test"]);
    assert!(delete.status.success(), "{delete:?}");
    let state = Command::new(bin_path("ocirun"))
        .args(["--root"])
        .arg(root_dir.path())
        .args(["state", "run-detach-keep-test"])
        .env_remove("OCI_TOOLS_LOG")
        .output()
        .expect("failed to spawn ocirun state");
    assert!(!state.status.success(), "{state:?}");
}

/// Writes a bundle just like [`write_bundle`], plus a `cat` applet
/// (needed to actually read `/proc/keys` back out) and `/proc/keys`
/// removed from the default `maskedPaths` list (`docs/design/0376`'s
/// research confirmed real runc/docker mask it by default for good
/// reason — a real container should never be able to enumerate every
/// key on the host — so this is deliberately only done for this one
/// test bundle's own verification purposes, never the shared default).
fn write_keyring_test_bundle(dir: &Path, busybox: &Path, args: &[&str]) {
    write_bundle(dir, busybox, args);
    let _ = std::os::unix::fs::symlink("busybox", dir.join("rootfs/bin/cat"));
    let config_path = dir.join("config.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    if let Some(masked) = config["linux"]["maskedPaths"].as_array_mut() {
        masked.retain(|p| p.as_str() != Some("/proc/keys"));
    }
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
}

/// `ocirun run` joins a fresh, container-scoped session keyring by
/// default, matching real `runc run`/`crun run`'s own unconditional
/// default (`docs/design/0378`) — a real, kernel-level verification,
/// not just "no error was returned": the container's own `/proc/keys`
/// (masking removed just for this one test, see
/// `write_keyring_test_bundle`'s own doc comment) must show a real
/// keyring literally named after the container's own id, proving a
/// genuinely new, uniquely-named keyring was actually joined rather
/// than silently inheriting whatever session keyring `ocirun` itself
/// happened to have (this project's own entire previous behavior,
/// before this flag/mechanism existed at all).
#[test]
fn run_joins_a_new_session_keyring_named_after_the_container_id_by_default() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    write_keyring_test_bundle(dir.path(), &busybox, &["/bin/sh", "-c", "cat /proc/keys"]);

    let out = ocirun_run(dir.path(), "keyring-default-test-id");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.lines().any(|line| line.contains("keyring")
            && line.trim_end().ends_with("keyring-default-test-id: empty")),
        "expected a real keyring named after the container's own id in /proc/keys, got: {stdout:?}"
    );
}

/// `ocirun run --no-new-keyring` skips joining a new keyring entirely
/// — matching real `runc run --no-new-keyring`/`crun run
/// --no-new-keyring` exactly: no keyring named after the container's
/// own id ever appears, a direct, real contrast with the default-case
/// test above.
#[test]
fn run_no_new_keyring_skips_joining_a_new_session_keyring() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    write_keyring_test_bundle(dir.path(), &busybox, &["/bin/sh", "-c", "cat /proc/keys"]);

    let out = Command::new(bin_path("ocirun"))
        .args(["run", "keyring-no-new-keyring-test-id", "--bundle"])
        .arg(dir.path())
        .current_dir(dir.path())
        .args(["--root"])
        .arg(dir.path().join("state-root"))
        .args(["--no-new-keyring"])
        .env_remove("OCI_TOOLS_LOG")
        .output()
        .expect("failed to spawn ocirun run --no-new-keyring");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("keyring-no-new-keyring-test-id"),
        "--no-new-keyring must not join a keyring named after the container's own id: {stdout:?}"
    );
}
