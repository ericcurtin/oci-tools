//! `ocirun run` lifecycle hook tests: all six real hook points this
//! project executes (see `docs/design/0026`/`0035`/`0087`).
//! `poststart`/`poststop`/`prestart`/`createRuntime` hook processes run
//! in the *runtime* namespace (the host, not the container), so a hook
//! can write to an ordinary host-side temp file to prove it ran and
//! with what state — no container rootfs involvement needed for the
//! hook side of things. `createContainer` also runs before
//! `pivot_root` (same as `prestart`/`createRuntime`), so
//! `add_dump_state_hook` works unchanged for it too;
//! `startContainer` runs *after* `pivot_root`, so those hooks write
//! into the container's own rootfs instead (see its own tests below).

use std::path::Path;
use std::process::Command;

use oci_tools_tests::{bin_path, busybox_path, write_bundle};

fn ocirun_run(dir: &Path, id: &str) -> std::process::Output {
    // `--bundle <dir>` explicitly (an absolute path, since `dir` always
    // is one here) rather than relying on `current_dir` + the default
    // `.`: `Bundle::path` is stored exactly as given, not canonicalized
    // (see its own doc comment), so a hook state assertion against the
    // bundle path needs a stable, meaningful value to check, not
    // literally `"."`.
    Command::new(bin_path("ocirun"))
        .args(["run", id, "--bundle"])
        .arg(dir)
        .env_remove("OCI_TOOLS_LOG")
        .output()
        .expect("failed to spawn ocirun run")
}

/// Add a `hooks.<point>` entry to an already-`write_bundle`-built
/// `config.json` that runs `/bin/sh -c "cat > <out>"` — dumping the
/// hook's own stdin (the state JSON) verbatim into `out`, a host-side
/// path the test can read back afterward.
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
fn poststart_hook_receives_a_running_state_with_a_real_pid() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    write_bundle(dir.path(), &busybox, &["/bin/sh", "-c", "true"]);
    let out = dir.path().join("poststart-state.json");
    add_dump_state_hook(dir.path(), "poststart", &out);

    let result = ocirun_run(dir.path(), "hooks-poststart-test");
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&out).unwrap()).expect("hook's stdin wasn't JSON");
    assert_eq!(state["id"], "hooks-poststart-test");
    assert_eq!(state["status"], "running");
    assert!(
        state["pid"].as_i64().unwrap() > 0,
        "expected a real pid: {state:?}"
    );
    assert_eq!(state["bundle"], dir.path().to_string_lossy().as_ref());
}

#[test]
fn poststop_hook_receives_a_stopped_state_after_the_container_exits() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    write_bundle(dir.path(), &busybox, &["/bin/sh", "-c", "true"]);
    let out = dir.path().join("poststop-state.json");
    add_dump_state_hook(dir.path(), "poststop", &out);

    let result = ocirun_run(dir.path(), "hooks-poststop-test");
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&out).unwrap()).expect("hook's stdin wasn't JSON");
    assert_eq!(state["id"], "hooks-poststop-test");
    assert_eq!(state["status"], "stopped");
    assert_eq!(state["pid"], 0, "pid should be 0 once stopped: {state:?}");
}

#[test]
fn a_failing_poststart_hook_does_not_change_the_containers_own_exit_code() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    write_bundle(dir.path(), &busybox, &["/bin/sh", "-c", "exit 42"]);
    let config_path = dir.path().join("config.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["hooks"] = serde_json::json!({
        "poststart": [{"path": "/bin/sh", "args": ["sh", "-c", "exit 1"]}]
    });
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();

    let result = ocirun_run(dir.path(), "hooks-failing-poststart-test");
    assert_eq!(
        result.status.code(),
        Some(42),
        "a failing poststart hook must not change the container's own exit code: {result:?}"
    );
}

#[test]
fn prestart_hook_receives_a_created_state_with_a_real_pid_before_the_container_runs() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    write_bundle(dir.path(), &busybox, &["/bin/sh", "-c", "true"]);
    let out = dir.path().join("prestart-state.json");
    add_dump_state_hook(dir.path(), "prestart", &out);

    let result = ocirun_run(dir.path(), "hooks-prestart-test");
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&out).unwrap()).expect("hook's stdin wasn't JSON");
    assert_eq!(state["id"], "hooks-prestart-test");
    assert_eq!(state["status"], "created");
    assert!(
        state["pid"].as_i64().unwrap() > 0,
        "expected a real pid: {state:?}"
    );
    assert_eq!(state["bundle"], dir.path().to_string_lossy().as_ref());
}

#[test]
fn create_runtime_hook_receives_a_created_state_too() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    write_bundle(dir.path(), &busybox, &["/bin/sh", "-c", "true"]);
    let out = dir.path().join("create-runtime-state.json");
    add_dump_state_hook(dir.path(), "createRuntime", &out);

    let result = ocirun_run(dir.path(), "hooks-create-runtime-test");
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&out).unwrap()).expect("hook's stdin wasn't JSON");
    assert_eq!(state["id"], "hooks-create-runtime-test");
    assert_eq!(state["status"], "created");
}

#[test]
fn prestart_runs_before_create_runtime() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    write_bundle(dir.path(), &busybox, &["/bin/sh", "-c", "true"]);
    let log = dir.path().join("order.log");
    let config_path = dir.path().join("config.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["hooks"] = serde_json::json!({
        "prestart": [{"path": "/bin/sh", "args": ["sh", "-c", format!("echo prestart >> {}", log.display())]}],
        "createRuntime": [{"path": "/bin/sh", "args": ["sh", "-c", format!("echo createRuntime >> {}", log.display())]}],
    });
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();

    let result = ocirun_run(dir.path(), "hooks-order-test");
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let order = std::fs::read_to_string(&log).unwrap();
    assert_eq!(
        order, "prestart\ncreateRuntime\n",
        "prestart must run, and finish, before createRuntime"
    );
}

#[test]
fn a_failing_prestart_hook_aborts_the_container_entirely() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    write_bundle(dir.path(), &busybox, &["/bin/sh", "-c", "true"]);
    let config_path = dir.path().join("config.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    let should_not_run = dir.path().join("should-not-run");
    config["hooks"] = serde_json::json!({
        "prestart": [{"path": "/bin/sh", "args": ["sh", "-c", "exit 5"]}],
        "createRuntime": [{"path": "/bin/sh", "args": ["sh", "-c", format!("touch {}", should_not_run.display())]}],
    });
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();

    let result = ocirun_run(dir.path(), "hooks-failing-prestart-test");
    assert!(
        !result.status.success(),
        "a failing prestart hook must fail the whole `ocirun run`: {result:?}"
    );
    assert!(
        !should_not_run.exists(),
        "createRuntime must never run once prestart has already failed"
    );
}

#[test]
fn create_container_hook_receives_a_creating_state_with_host_paths_still_visible() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    write_bundle(dir.path(), &busybox, &["/bin/sh", "-c", "true"]);
    // A real, host-only path -- `createContainer` runs before
    // `pivot_root`, so it still sees the same filesystem view the
    // runtime process itself does (matching real runc's own
    // `s.Status = specs.StateCreating`, checked directly against
    // `~/git/runc/libcontainer/rootfs_linux.go`, not assumed).
    let out = dir.path().join("create-container-state.json");
    add_dump_state_hook(dir.path(), "createContainer", &out);

    let result = ocirun_run(dir.path(), "hooks-create-container-test");
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&out).unwrap()).expect("hook's stdin wasn't JSON");
    assert_eq!(state["id"], "hooks-create-container-test");
    assert_eq!(state["status"], "creating");
    assert!(
        state["pid"].as_i64().unwrap() > 0,
        "expected a real pid: {state:?}"
    );
    assert_eq!(state["bundle"], dir.path().to_string_lossy().as_ref());
}

#[test]
fn start_container_hook_receives_a_created_state_and_runs_inside_the_containers_own_rootfs() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    write_bundle(dir.path(), &busybox, &["/bin/sh", "-c", "true"]);
    // `startContainer` runs *after* `pivot_root`: it sees the
    // container's own root, not the host's -- checked directly against
    // a real kernel first (see `docs/design/0087`), not assumed. A
    // real, host-only path like `dir.path()` would not be reachable
    // from here at all; the hook instead writes to `/` (its own,
    // already-pivoted root), which the test reads back afterward at
    // `rootfs/...` from the host side, since it's the very same
    // underlying directory.
    let config_path = dir.path().join("config.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    // The default rootless spec sets `root.readonly: true`; a hook
    // that needs to write into the container's own rootfs needs it
    // writable, same as the container's own command would.
    config["root"]["readonly"] = serde_json::json!(false);
    config["hooks"] = serde_json::json!({
        "startContainer": [{"path": "/bin/sh", "args": ["sh", "-c", "cat > /start-container-state.json"]}]
    });
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();

    let result = ocirun_run(dir.path(), "hooks-start-container-test");
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let out = dir.path().join("rootfs/start-container-state.json");
    let state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&out).unwrap()).expect("hook's stdin wasn't JSON");
    assert_eq!(state["id"], "hooks-start-container-test");
    assert_eq!(state["status"], "created");
    assert!(
        state["pid"].as_i64().unwrap() > 0,
        "expected a real pid: {state:?}"
    );
}

#[test]
fn a_failing_create_container_hook_aborts_the_container_and_start_container_never_runs() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    write_bundle(dir.path(), &busybox, &["/bin/sh", "-c", "true"]);
    let config_path = dir.path().join("config.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["root"]["readonly"] = serde_json::json!(false);
    config["hooks"] = serde_json::json!({
        "createContainer": [{"path": "/bin/sh", "args": ["sh", "-c", "exit 9"]}],
        "startContainer": [{"path": "/bin/sh", "args": ["sh", "-c", "touch /should-not-run"]}],
    });
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();

    let result = ocirun_run(dir.path(), "hooks-failing-create-container-test");
    assert!(
        !result.status.success(),
        "a failing createContainer hook must fail the whole `ocirun run`: {result:?}"
    );
    assert!(
        !dir.path().join("rootfs/should-not-run").exists(),
        "startContainer must never run once createContainer has already failed"
    );
}

#[test]
fn create_container_runs_before_start_container() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    write_bundle(dir.path(), &busybox, &["/bin/sh", "-c", "true"]);
    let config_path = dir.path().join("config.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["root"]["readonly"] = serde_json::json!(false);
    // `createContainer` runs pre-pivot (host paths visible);
    // `startContainer` runs post-pivot (container paths only) -- so
    // the shared order log has to live inside the container's own
    // rootfs, reachable from both sides either way.
    config["hooks"] = serde_json::json!({
        "createContainer": [{"path": "/bin/sh", "args": ["sh", "-c", format!("echo createContainer >> {}/rootfs/order.log", dir.path().display())]}],
        "startContainer": [{"path": "/bin/sh", "args": ["sh", "-c", "echo startContainer >> /order.log"]}],
    });
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();

    let result = ocirun_run(dir.path(), "hooks-create-start-order-test");
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let order = std::fs::read_to_string(dir.path().join("rootfs/order.log")).unwrap();
    assert_eq!(
        order, "createContainer\nstartContainer\n",
        "createContainer must run, and finish, before startContainer"
    );
}

#[test]
fn a_container_without_hooks_configured_still_runs_normally() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    write_bundle(dir.path(), &busybox, &["/bin/echo", "no-hooks-needed"]);

    let result = ocirun_run(dir.path(), "hooks-none-test");
    assert!(
        result.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        String::from_utf8_lossy(&result.stdout).contains("no-hooks-needed"),
        "got: {result:?}"
    );
}
