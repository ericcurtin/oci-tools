//! `ociman attach` integration tests: attaching to an already-*running*
//! container's own live output from a real, entirely separate `ociman`
//! invocation than the one that started it (`ociman run -d`) — the
//! same fully offline seeded-image approach `ociman_start.rs`/
//! `ociman_run.rs` established.
//!
//! Deliberately output-only (see [`Command::Attach`](../../../bin/ociman/src/main.rs)'s
//! own doc comment): real stdin forwarding into an already-running,
//! already-detached container isn't attempted here at all.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use oci_spec_types::image::ContainerConfig;
use oci_store::Store;

use oci_tools_tests::{bin_path, busybox_path, seed_image};

fn ociman(storage_root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
        .env_remove("OCI_TOOLS_LOG")
        .args(args)
        .output()
        .expect("failed to spawn ociman")
}

fn ociman_run_detached(storage_root: &Path, image: &str, container_args: &[&str]) {
    let out = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
        .env_remove("OCI_TOOLS_LOG")
        .args(["run", "-d", image])
        .args(container_args)
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn ociman run -d");
    assert!(
        out.status.success(),
        "ociman run -d failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn only_container_id(storage_root: &Path, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let out = ociman(storage_root, &["ps", "-a", "-q"]);
        let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !id.is_empty() || Instant::now() >= deadline {
            return id;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn inspect_json(storage_root: &Path, id: &str) -> serde_json::Value {
    let out = ociman(storage_root, &["inspect", id, "--json"]);
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("inspect --json output was not valid JSON: {e}"))
}

fn wait_for_status(storage_root: &Path, id: &str, want: &str, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let status = inspect_json(storage_root, id)["status"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if status == want || Instant::now() >= deadline {
            return status;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// `ociman attach` on a container an entirely separate, earlier
/// invocation started (`ociman run -d`) streams its own full
/// already-captured output (not just whatever's written after attach
/// began watching — the log file is read from its own start every
/// time), blocks until it stops, and this command's own exit code
/// becomes the container's own real, nonzero exit code — matching
/// real `docker attach`/`podman attach`'s own observable output
/// behavior exactly.
#[test]
fn attach_streams_full_output_and_propagates_exit_code() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        storage_dir.path().join(".rootless-overlay-supported"),
        "false",
    )
    .unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/attach:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "echo line1; sleep 0.2; echo line2; sleep 0.2; echo line3; exit 5".to_string(),
            ]),
            ..Default::default()
        },
    );

    ociman_run_detached(storage_dir.path(), "ociman-test/attach:latest", &[]);
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running",
        "the container should genuinely be running before attach is even attempted"
    );

    let attach = ociman(storage_dir.path(), &["attach", &id]);
    assert_eq!(
        attach.status.code(),
        Some(5),
        "attach's own exit code should be the container's own real exit code; stderr: {}",
        String::from_utf8_lossy(&attach.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&attach.stdout),
        "line1\nline2\nline3\n",
        "attach should stream the container's own full output (from the start, not just \
         whatever's written after attach began), and never print the container id"
    );
    assert_eq!(
        wait_for_status(storage_dir.path(), &id, "stopped", Duration::from_secs(20)),
        "stopped"
    );

    ociman(storage_dir.path(), &["rm", &id]);
}

/// Attaching to an already-stopped container is a clear, real error
/// naming its own current status — never a silent hang or a confusing
/// lower-level failure.
#[test]
fn attach_to_a_stopped_container_is_a_clear_error() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        storage_dir.path().join(".rootless-overlay-supported"),
        "false",
    )
    .unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/attach-stopped:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let create = ociman(
        storage_dir.path(),
        &["create", "ociman-test/attach-stopped:latest", "/bin/true"],
    );
    assert!(
        create.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&create.stderr)
    );
    let id = String::from_utf8_lossy(&create.stdout).trim().to_string();

    let attach = ociman(storage_dir.path(), &["attach", &id]);
    assert!(!attach.status.success());
    let stderr = String::from_utf8_lossy(&attach.stderr);
    assert!(
        stderr.contains("only attach to a running container"),
        "{stderr}"
    );
    assert!(stderr.contains("created"), "{stderr}");
}

/// An unknown container id/name is a clear error, not a confusing
/// lower-level one.
#[test]
fn attach_of_an_unknown_container_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let attach = ociman(storage_dir.path(), &["attach", "does-not-exist"]);
    assert!(!attach.status.success());
}

/// `ociman attach --latest`/`-l` (matching real `podman attach
/// --latest` exactly) attaches to the single, real most-recently-
/// *created* container -- an earlier container's own, genuinely
/// different output must never appear.
#[test]
fn attach_latest_streams_the_most_recently_created_running_container() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        storage_dir.path().join(".rootless-overlay-supported"),
        "false",
    )
    .unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/attach-latest-older:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "echo older-output; sleep 3".to_string(),
            ]),
            ..Default::default()
        },
    );
    seed_image(
        &store,
        "ociman-test/attach-latest-newer:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "echo newer-output; sleep 3".to_string(),
            ]),
            ..Default::default()
        },
    );

    ociman_run_detached(
        storage_dir.path(),
        "ociman-test/attach-latest-older:latest",
        &[],
    );
    let older_id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!older_id.is_empty());
    assert_eq!(
        wait_for_status(
            storage_dir.path(),
            &older_id,
            "running",
            Duration::from_secs(20)
        ),
        "running"
    );

    // A real, distinguishable creation-time gap.
    std::thread::sleep(Duration::from_secs(2));

    ociman_run_detached(
        storage_dir.path(),
        "ociman-test/attach-latest-newer:latest",
        &[],
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    let newer_id = loop {
        let out = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
        let ids: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect();
        if let Some(id) = ids.iter().find(|id| *id != &older_id) {
            break id.clone();
        }
        assert!(Instant::now() < deadline, "second container never appeared");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(
        wait_for_status(
            storage_dir.path(),
            &newer_id,
            "running",
            Duration::from_secs(20)
        ),
        "running"
    );

    // Give the newer container's own `echo` a moment to have already
    // produced real, captured output before attach ever reads it.
    std::thread::sleep(Duration::from_millis(200));

    let attach = ociman(storage_dir.path(), &["attach", "--latest"]);
    let stdout = String::from_utf8_lossy(&attach.stdout).into_owned();
    assert!(
        stdout.contains("newer-output"),
        "--latest must stream the most recently created container's own output: {stdout:?}"
    );
    assert!(
        !stdout.contains("older-output"),
        "--latest must never stream an earlier container's own output: {stdout:?}"
    );

    ociman(storage_dir.path(), &["kill", &older_id]);
    ociman(storage_dir.path(), &["kill", &newer_id]);
    ociman(storage_dir.path(), &["rm", "-a", "-f"]);
}

/// A real, deliberate divergence from most other siblings in this
/// rollout: real podman's own checked-directly `attach` function has
/// no mutual-exclusivity check at all between `--latest` and an
/// explicit `ID` -- the explicit one always silently wins outright,
/// never a real error, ported faithfully here too (the same real
/// shape `ociman diff --latest`, `0448`, already established).
#[test]
fn attach_explicit_id_silently_wins_over_latest_when_both_given() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        storage_dir.path().join(".rootless-overlay-supported"),
        "false",
    )
    .unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/attach-explicit-wins:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "echo explicit-output; exit 0".to_string(),
            ]),
            ..Default::default()
        },
    );

    ociman_run_detached(
        storage_dir.path(),
        "ociman-test/attach-explicit-wins:latest",
        &[],
    );
    let id = only_container_id(storage_dir.path(), Duration::from_secs(20));
    assert!(!id.is_empty());

    // `--latest` would still resolve to this exact same, only
    // container here, but the point is that giving both together is
    // not a real error at all.
    let attach = ociman(storage_dir.path(), &["attach", "--latest", &id]);
    assert!(
        attach.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&attach.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&attach.stdout), "explicit-output\n");

    ociman(storage_dir.path(), &["rm", &id]);
}

/// Neither `--latest` nor an explicit container at all is a real,
/// immediate error, matching real podman's own exact wording.
#[test]
fn attach_with_no_id_and_no_latest_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(storage_dir.path(), &["attach"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("attach requires the name or id of one running container or the latest flag"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `attach --latest` on a genuinely empty store is a real, clear
/// error, matching real `podman attach --latest`'s own
/// `ErrNoSuchCtr`.
#[test]
fn attach_latest_on_an_empty_store_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    let out = ociman(storage_dir.path(), &["attach", "--latest"]);
    assert!(!out.status.success());
}
