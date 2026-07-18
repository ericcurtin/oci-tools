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
