//! `ociman run --name` integration tests: a human-chosen name usable
//! anywhere the generated short id is (`ps`/`rm`/`stop`/`exec`/`logs`),
//! matching real `docker run --name`/`podman run --name` (see
//! `docs/design/0032`). Same fully offline seeded-image approach
//! `ociman_run.rs` established.

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

fn ociman_run_detached(
    storage_root: &Path,
    image: &str,
    container_args: &[&str],
) -> std::process::Child {
    Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
        .env_remove("OCI_TOOLS_LOG")
        .args(["run", image])
        .args(container_args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn ociman run")
}

/// A generous timeout: `ociman run` now attempts a real systemd cgroup
/// driver D-Bus round trip per container (`docs/design/0034`), which
/// can occasionally take noticeably longer under heavy *concurrent*
/// test-suite load — the ordinary case still resolves in milliseconds.
fn wait_for_container_status(
    storage_root: &Path,
    id: &str,
    want: &str,
    timeout: Duration,
) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let out = ociman(storage_root, &["ps", "-a", "--json"]);
        if out.status.success()
            && let Ok(views) = serde_json::from_slice::<serde_json::Value>(&out.stdout)
            && let Some(entry) = views
                .as_array()
                .and_then(|a| a.iter().find(|e| e["id"] == id))
        {
            let status = entry["status"].as_str().unwrap_or_default().to_string();
            if status == want || Instant::now() >= deadline {
                return status;
            }
        } else if Instant::now() >= deadline {
            return String::new();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn run_name_shows_up_in_ps_and_rm_accepts_the_name() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/name-basic:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "exit 0".to_string(),
            ]),
            ..Default::default()
        },
    );

    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "--name",
            "my-container",
            "ociman-test/name-basic:latest",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    let ps = ociman(storage_dir.path(), &["ps", "-a", "--json"]);
    let views: serde_json::Value = serde_json::from_slice(&ps.stdout).unwrap();
    let entry = &views[0];
    assert_eq!(entry["name"], "my-container");
    let id = entry["id"].as_str().unwrap().to_string();

    // `rm` by name.
    let rm = ociman(storage_dir.path(), &["rm", "my-container"]);
    assert!(
        rm.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rm.stderr)
    );
    // Echoes back what was given (the name), matching how `rm`/`stop`
    // already just echo back their own argument, not a resolved id.
    assert_eq!(String::from_utf8_lossy(&rm.stdout).trim(), "my-container");

    let ps_after = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    assert!(
        !String::from_utf8_lossy(&ps_after.stdout).contains(&id),
        "container should be gone after rm by name"
    );
}

#[test]
fn run_name_must_be_unique_among_existing_containers() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/name-unique:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "exit 0".to_string(),
            ]),
            ..Default::default()
        },
    );

    let first = ociman(
        storage_dir.path(),
        &[
            "run",
            "--name",
            "dup-name",
            "ociman-test/name-unique:latest",
        ],
    );
    assert!(first.status.success());

    // A second container with the same name must be refused, even
    // though the first is now stopped (a stopped container still
    // holds its name until removed, matching real docker/podman).
    let second = ociman(
        storage_dir.path(),
        &[
            "run",
            "--name",
            "dup-name",
            "ociman-test/name-unique:latest",
        ],
    );
    assert!(
        !second.status.success(),
        "a duplicate --name should be refused"
    );
    assert!(
        String::from_utf8_lossy(&second.stderr).contains("already in use"),
        "got stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );

    // Only one container exists.
    let ps = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    assert_eq!(
        String::from_utf8_lossy(&ps.stdout).trim().lines().count(),
        1
    );
}

/// `--replace` (matching real `podman run --replace`/`podman create
/// --replace` exactly, checked directly against
/// `~/git/podman/cmd/podman/containers/create.go`'s own
/// `replaceContainer`): a second `run --name X --replace` genuinely
/// removes the first, already-*stopped* container of that same name
/// instead of erroring, leaving exactly one container behind (a real,
/// different id -- proving the old one was truly removed and a new
/// one created, not merely left alone).
#[test]
fn run_replace_removes_an_existing_stopped_container_of_the_same_name() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/replace-stopped:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "exit 0".to_string(),
            ]),
            ..Default::default()
        },
    );

    let first = ociman(
        storage_dir.path(),
        &[
            "run",
            "--name",
            "replace-me",
            "ociman-test/replace-stopped:latest",
        ],
    );
    assert!(first.status.success());
    let first_id = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    let first_id = String::from_utf8_lossy(&first_id.stdout).trim().to_string();

    let second = ociman(
        storage_dir.path(),
        &[
            "run",
            "--replace",
            "--name",
            "replace-me",
            "ociman-test/replace-stopped:latest",
        ],
    );
    assert!(
        second.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );

    let ps = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    let stdout = String::from_utf8_lossy(&ps.stdout).trim().to_string();
    let ids: Vec<&str> = stdout.lines().collect();
    assert_eq!(ids.len(), 1, "exactly one container should remain: {ids:?}");
    assert_ne!(
        ids[0], first_id,
        "the replaced container must be a genuinely new id, not the original left alone"
    );
}

/// The originally-hit real bug this flag's own implementation found
/// and fixed along the way (a real fork-safety hazard, the identical
/// class `docs/design/0159` already found/fixed for `ociman restart`):
/// replacing a container that is still genuinely *running* (not just
/// stopped) must not panic or corrupt anything -- `remove_container`'s
/// own force-kill path spawns a background D-Bus thread for its own
/// best-effort failed-scope cleanup, which must be deferred past this
/// same process's own next real `fork()` (the brand new detached
/// container's own launch), not performed immediately.
#[test]
fn run_replace_removes_an_existing_running_container_of_the_same_name() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/replace-running:latest",
        &busybox,
        &["sh", "sleep"],
        ContainerConfig::default(),
    );

    let first = ociman(
        storage_dir.path(),
        &[
            "run",
            "-d",
            "--name",
            "replace-me-running",
            "ociman-test/replace-running:latest",
            "sleep",
            "60",
        ],
    );
    assert!(
        first.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first_id = String::from_utf8_lossy(&first.stdout).trim().to_string();
    assert_eq!(
        ociman_status(storage_dir.path(), &first_id),
        "running",
        "the first container must genuinely be running before the replace attempt"
    );

    let second = ociman(
        storage_dir.path(),
        &[
            "run",
            "-d",
            "--replace",
            "--name",
            "replace-me-running",
            "ociman-test/replace-running:latest",
            "sleep",
            "60",
        ],
    );
    assert!(
        second.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    let second_id = String::from_utf8_lossy(&second.stdout).trim().to_string();
    assert_ne!(second_id, first_id);
    assert_eq!(ociman_status(storage_dir.path(), &second_id), "running");

    let ps = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    assert_eq!(
        String::from_utf8_lossy(&ps.stdout).trim().lines().count(),
        1,
        "exactly one container should remain"
    );

    let kill = ociman(storage_dir.path(), &["kill", &second_id]);
    assert!(kill.status.success());
}

/// Matching real podman's own exact wording, checked directly
/// (`~/git/podman/cmd/podman/containers/create.go`'s own
/// `replaceContainer`): `--replace` with no `--name` at all has
/// nothing to identify, so it's a real, immediate error, not silently
/// ignored.
#[test]
fn run_replace_without_name_is_a_clear_error() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/replace-no-name:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let out = ociman(
        storage_dir.path(),
        &["run", "--replace", "ociman-test/replace-no-name:latest"],
    );
    assert!(!out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stderr).trim(),
        "error: cannot replace container without --name being set"
    );
}

/// `ociman create --replace`, matching real `podman create --replace`
/// -- `cmd_create` never forks any real process of its own at all
/// (this project's own deliberately lazy `create`), so the deferred-
/// scope-reset handoff `run_replace_removes_an_existing_running_
/// container_of_the_same_name` exercises for `run -d` is genuinely
/// moot here, but the actual replace-and-recreate behavior must still
/// work identically.
#[test]
fn create_replace_removes_an_existing_container_of_the_same_name() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/create-replace:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "exit 0".to_string(),
            ]),
            ..Default::default()
        },
    );

    let first = ociman(
        storage_dir.path(),
        &[
            "create",
            "--name",
            "create-replace-me",
            "ociman-test/create-replace:latest",
        ],
    );
    assert!(first.status.success());
    let first_id = String::from_utf8_lossy(&first.stdout).trim().to_string();

    let second = ociman(
        storage_dir.path(),
        &[
            "create",
            "--replace",
            "--name",
            "create-replace-me",
            "ociman-test/create-replace:latest",
        ],
    );
    assert!(
        second.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    let second_id = String::from_utf8_lossy(&second.stdout).trim().to_string();
    assert_ne!(second_id, first_id);

    let ps = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    assert_eq!(
        String::from_utf8_lossy(&ps.stdout).trim().lines().count(),
        1
    );
}

/// The status field from `ociman --json ps -a`, asserting the command
/// itself succeeded and that `id` is present.
fn ociman_status(storage_root: &Path, id: &str) -> String {
    let out = ociman(storage_root, &["--json", "ps", "-a"]);
    assert!(out.status.success());
    let views: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let view = views
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"].as_str() == Some(id))
        .unwrap_or_else(|| panic!("{id:?} not found in ociman ps -a output"));
    view["status"].as_str().unwrap().to_string()
}

#[test]
fn run_rejects_an_invalid_name() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/name-invalid:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let out = ociman(
        storage_dir.path(),
        &[
            "run",
            "--name",
            "name/with/slashes",
            "ociman-test/name-invalid:latest",
        ],
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("invalid container name"));
}

#[test]
fn logs_and_exec_accept_a_name_instead_of_an_id() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/name-logs-exec:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let mut run = ociman_run_detached(
        storage_dir.path(),
        "ociman-test/name-logs-exec:latest",
        &["--name", "named-runner", "/bin/sh", "-c", "sleep 5"],
    );
    // `ociman run --name X <image> <args>`: clap parses `--name` before
    // the positional `image`/trailing args regardless of where it
    // appears on the command line, so this ordering (name before
    // image) is exactly equivalent to the more common `--name X image
    // args` form used elsewhere in this file.
    let id = {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let out = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
            let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !id.is_empty() || Instant::now() >= deadline {
                break id;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    assert!(!id.is_empty());
    assert_eq!(
        wait_for_container_status(storage_dir.path(), &id, "running", Duration::from_secs(20)),
        "running"
    );

    let exec = ociman(
        storage_dir.path(),
        &[
            "exec",
            "named-runner",
            "/bin/sh",
            "-c",
            "echo exec-by-name-worked",
        ],
    );
    assert!(
        exec.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&exec.stderr)
    );
    assert!(
        String::from_utf8_lossy(&exec.stdout).contains("exec-by-name-worked"),
        "got: {:?}",
        exec.stdout
    );

    let logs = ociman(storage_dir.path(), &["logs", "named-runner"]);
    assert!(
        logs.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&logs.stderr)
    );

    ociman(storage_dir.path(), &["stop", "named-runner"]);
    run.wait().unwrap();
    ociman(storage_dir.path(), &["rm", "named-runner"]);
}

#[test]
fn an_unknown_name_is_reported_the_same_way_as_an_unknown_id() {
    let storage_dir = tempfile::tempdir().unwrap();
    let _store = Store::open(storage_dir.path()).unwrap();

    let out = ociman(storage_dir.path(), &["rm", "no-such-name"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("does not exist"));
}
