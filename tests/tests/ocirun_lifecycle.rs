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

/// Add a `hooks.<point>` entry to an already-`write_bundle`-built
/// `config.json` that runs `/bin/sh -c "cat > <out>"` — dumping the
/// hook's own stdin (the state JSON) verbatim into `out`. Same helper
/// `ocirun_hooks.rs`'s own tests already use (kept as a small,
/// deliberate per-file duplicate rather than a shared, cross-file
/// export: neither file otherwise depends on the other).
fn add_dump_state_hook(bundle_dir: &Path, point: &str, out: &Path) {
    let config_path = bundle_dir.join("config.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["hooks"] = serde_json::json!({
        point: [{
            "path": "/bin/sh",
            "args": ["sh", "-c", format!("cat > {}", out.display())],
        }]
    });
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
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

/// `create` runs `prestart` then `createRuntime` synchronously, before
/// ever returning — matching real runc's own `Container.Start`/`Run`
/// (see `docs/design/0089`). Both write to the same shared,
/// host-side order log (real `prestart`/`createRuntime` hooks run in
/// the *runtime* namespace, same as `run`'s own equivalent test in
/// `ocirun_hooks.rs`).
#[test]
fn create_runs_prestart_then_create_runtime_before_returning() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "sleep 30"]);
    let log = bundle_dir.path().join("order.log");
    let config_path = bundle_dir.path().join("config.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["hooks"] = serde_json::json!({
        "prestart": [{"path": "/bin/sh", "args": ["sh", "-c", format!("echo prestart >> {}", log.display())]}],
        "createRuntime": [{"path": "/bin/sh", "args": ["sh", "-c", format!("echo createRuntime >> {}", log.display())]}],
    });
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();

    let create = ocirun_create(root_dir.path(), bundle_dir.path(), "create-hooks-test");
    assert!(
        create.status.success(),
        "create failed: {}",
        String::from_utf8_lossy(&create.stderr)
    );
    // Already true by the time `create` itself returns -- no polling
    // needed, unlike hooks that run inside the container's own process.
    let order = std::fs::read_to_string(&log).unwrap();
    assert_eq!(
        order, "prestart\ncreateRuntime\n",
        "both hooks must have already run, in order, before create returns"
    );

    let kill = ocirun(root_dir.path(), &["kill", "create-hooks-test", "KILL"]);
    assert!(kill.status.success());
    wait_for_status(
        root_dir.path(),
        "create-hooks-test",
        "stopped",
        Duration::from_secs(5),
    );
    ocirun(root_dir.path(), &["delete", "create-hooks-test"]);
}

/// A failing `prestart` hook aborts `create` outright (matching real
/// runc: `c.start()` returns the hook's own error, so `Container.
/// Start`/`Run` never even reports a pid) -- no lingering state, no
/// container process left behind.
#[test]
fn a_failing_prestart_hook_aborts_create_entirely() {
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
    config["hooks"] = serde_json::json!({
        "prestart": [{"path": "/bin/sh", "args": ["sh", "-c", "exit 3"]}],
    });
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();

    let create = ocirun_create(root_dir.path(), bundle_dir.path(), "create-fail-hooks-test");
    assert!(
        !create.status.success(),
        "a failing prestart hook must fail create: {create:?}"
    );

    let after = ocirun(root_dir.path(), &["state", "create-fail-hooks-test"]);
    assert!(
        !after.status.success(),
        "no state should have been left behind after a failed create"
    );
}

/// `start` runs `poststart` right after signalling the exec fifo, with
/// a `"running"` status and the same real pid `ocirun state` reports —
/// matching real runc's own `Container.exec()` (see `docs/design/0089`).
#[test]
fn start_runs_poststart_hook_with_a_running_state() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "sleep 30"]);
    let out = bundle_dir.path().join("poststart-state.json");
    add_dump_state_hook(bundle_dir.path(), "poststart", &out);

    ocirun_create(root_dir.path(), bundle_dir.path(), "start-poststart-test");
    let real_pid: i64 = {
        let state = ocirun(root_dir.path(), &["state", "start-poststart-test"]);
        let json: serde_json::Value = serde_json::from_slice(&state.stdout).unwrap();
        json["pid"].as_i64().unwrap()
    };

    let start = ocirun(root_dir.path(), &["start", "start-poststart-test"]);
    assert!(
        start.status.success(),
        "start failed: {}",
        String::from_utf8_lossy(&start.stderr)
    );

    let state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&out).unwrap()).expect("hook's stdin wasn't JSON");
    assert_eq!(state["id"], "start-poststart-test");
    assert_eq!(state["status"], "running");
    assert_eq!(state["pid"].as_i64().unwrap(), real_pid);

    let kill = ocirun(root_dir.path(), &["kill", "start-poststart-test", "KILL"]);
    assert!(kill.status.success());
    wait_for_status(
        root_dir.path(),
        "start-poststart-test",
        "stopped",
        Duration::from_secs(5),
    );
    ocirun(root_dir.path(), &["delete", "start-poststart-test"]);
}

/// `delete` runs `poststop` with a `"stopped"` status and `pid: 0` —
/// matching real runc's own `destroy()`, which always runs it as part
/// of tearing a container down (see `docs/design/0089`).
#[test]
fn delete_runs_poststop_hook_with_a_stopped_state() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "sleep 30"]);
    let out = bundle_dir.path().join("poststop-state.json");
    add_dump_state_hook(bundle_dir.path(), "poststop", &out);

    ocirun_create(root_dir.path(), bundle_dir.path(), "delete-poststop-test");
    let start = ocirun(root_dir.path(), &["start", "delete-poststop-test"]);
    assert!(start.status.success());
    let kill = ocirun(root_dir.path(), &["kill", "delete-poststop-test", "KILL"]);
    assert!(kill.status.success());
    wait_for_status(
        root_dir.path(),
        "delete-poststop-test",
        "stopped",
        Duration::from_secs(5),
    );

    let delete = ocirun(root_dir.path(), &["delete", "delete-poststop-test"]);
    assert!(
        delete.status.success(),
        "delete failed: {}",
        String::from_utf8_lossy(&delete.stderr)
    );

    let state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&out).unwrap()).expect("hook's stdin wasn't JSON");
    assert_eq!(state["id"], "delete-poststop-test");
    assert_eq!(state["status"], "stopped");
    assert_eq!(state["pid"], 0);
}

/// `create --pid-file` writes the real container pid, atomically
/// (never observable half-written — checked by simply reading it back
/// once `create` itself has returned, which only happens after the
/// real rename), matching real `runc create --pid-file` exactly. The
/// same real `state.pid` this project's own `ocirun state` already
/// reports is what the file should contain.
#[test]
fn create_pid_file_writes_the_real_pid() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    let pid_file = bundle_dir.path().join("container.pid");
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "sleep 30"]);

    let create = Command::new(bin_path("ocirun"))
        .arg("--root")
        .arg(root_dir.path())
        .args(["create", "pid-file-test", "--bundle"])
        .arg(bundle_dir.path())
        .args(["--pid-file"])
        .arg(&pid_file)
        .env_remove("OCI_TOOLS_LOG")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .expect("failed to spawn ocirun create");
    assert!(
        create.status.success(),
        "create failed: {}",
        String::from_utf8_lossy(&create.stderr)
    );
    assert_eq!(state_status(root_dir.path(), "pid-file-test"), "created");

    let state = ocirun(root_dir.path(), &["state", "pid-file-test"]);
    let state_json: serde_json::Value = serde_json::from_slice(&state.stdout).unwrap();
    let real_pid = state_json["pid"].as_i64().unwrap();

    let file_content = std::fs::read_to_string(&pid_file).unwrap();
    assert_eq!(
        file_content,
        real_pid.to_string(),
        "--pid-file's own content should be exactly the bare decimal pid, no trailing newline, \
         matching real runc's own createPidFile"
    );

    // Cleanup.
    let kill = ocirun(root_dir.path(), &["kill", "pid-file-test", "KILL"]);
    assert!(kill.status.success());
    wait_for_status(
        root_dir.path(),
        "pid-file-test",
        "stopped",
        Duration::from_secs(5),
    );
    ocirun(root_dir.path(), &["delete", "pid-file-test"]);
}

/// `run --pid-file` writes the same real pid while the container is
/// still running in the foreground -- checked by polling for the
/// file's own appearance (real `run` blocks in the foreground, so this
/// test spawns it detached, same reasoning `ocirun_create`'s own doc
/// comment already established for `create`).
#[test]
fn run_pid_file_writes_the_real_pid() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let bundle_dir = tempfile::tempdir().unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    let pid_file = bundle_dir.path().join("container.pid");
    write_bundle(bundle_dir.path(), &busybox, &["/bin/sh", "-c", "sleep 30"]);

    let mut run = Command::new(bin_path("ocirun"))
        .arg("--root")
        .arg(root_dir.path())
        .args(["run", "pid-file-run-test", "--bundle"])
        .arg(bundle_dir.path())
        .args(["--pid-file"])
        .arg(&pid_file)
        .env_remove("OCI_TOOLS_LOG")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn ocirun run");

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !pid_file.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(pid_file.exists(), "--pid-file was never written");

    let file_content = std::fs::read_to_string(&pid_file).unwrap();
    let pid: i32 = file_content
        .parse()
        .unwrap_or_else(|_| panic!("not a bare decimal pid: {file_content:?}"));
    // A real, live pid at this point -- `kill(pid, 0)` (no real signal
    // sent, `rustix::process::test_kill_process`) succeeds only if
    // `pid` is a real, currently-running process, the most direct
    // proof this project's own `ocirun` process actually wrote its
    // own real container pid, not a placeholder.
    let real_pid = rustix::process::Pid::from_raw(pid).expect("a real pid is never 0");
    assert!(
        rustix::process::test_kill_process(real_pid).is_ok(),
        "pid {pid} from --pid-file is not a live process"
    );

    // Cleanup: real `runc run`'s own container is this process's own
    // foreground child (the container's init), so killing the
    // reported container pid directly ends the whole thing; `run`
    // itself then exits with the container's own signal-death exit
    // code.
    let _ = rustix::process::kill_process(real_pid, rustix::process::Signal::KILL);
    let _ = run.wait();
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
