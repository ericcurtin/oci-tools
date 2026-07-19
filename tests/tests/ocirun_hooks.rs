//! `ocirun run` lifecycle hook tests: `poststart`/`poststop`, the only
//! two of the six real hook points this project executes yet (see
//! `docs/design/0026`). Real hook processes run in the *runtime*
//! namespace (the host, not the container), so a hook can write to an
//! ordinary host-side temp file to prove it ran and with what state —
//! no container rootfs involvement needed for the hook side of things.

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
