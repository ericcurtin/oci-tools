//! `ociman container clone` integration tests (`docs/design/0474`):
//! this project's own first, deliberately narrower slice of real
//! `podman container clone` — always clones from the exact same image
//! the source container itself already used (no positional `IMAGE`
//! override yet), a fresh, independent rootfs extracted from that
//! image, a byte-for-byte copy of the source's own current
//! `config.json` otherwise, and always left `Created` unless `--run`
//! says otherwise.

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
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn seed(storage_root: &Path, image: &str) {
    let busybox = busybox_path().expect("busybox not found on $PATH");
    let store = Store::open(storage_root).unwrap();
    seed_image(
        &store,
        image,
        &busybox,
        &["sh", "true", "sleep"],
        ContainerConfig::default(),
    );
}

#[test]
fn clone_creates_a_new_created_container_from_the_same_image() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    seed(storage_dir.path(), "ociman-test/clone-basic:latest");

    let create = ociman(
        storage_dir.path(),
        &[
            "create",
            "--name",
            "source",
            "--label",
            "env=prod",
            "ociman-test/clone-basic:latest",
            "true",
        ],
    );
    assert!(create.status.success(), "{create:?}");
    let source_id = String::from_utf8_lossy(&create.stdout).trim().to_string();

    let clone = ociman(storage_dir.path(), &["container", "clone", "source"]);
    assert!(
        clone.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&clone.stderr)
    );
    let clone_id = String::from_utf8_lossy(&clone.stdout).trim().to_string();
    assert!(!clone_id.is_empty());
    assert_ne!(clone_id, source_id);

    // The clone is a real, separate `Created` container.
    let clone_json = inspect_json(storage_dir.path(), &clone_id);
    assert_eq!(clone_json["status"], "created");
    // Real, independent rootfs directories -- never sharing one.
    assert_ne!(
        clone_json["rootfs"],
        inspect_json(storage_dir.path(), &source_id)["rootfs"]
    );

    // The default name is `<source-name>-clone`.
    let by_name = ociman(storage_dir.path(), &["inspect", "source-clone", "--json"]);
    assert!(by_name.status.success(), "{by_name:?}");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&by_name.stdout).unwrap()["id"],
        clone_id
    );

    // The source itself is completely untouched.
    assert_eq!(
        inspect_json(storage_dir.path(), &source_id)["status"],
        "created"
    );
}

/// A second `clone` (no explicit `--name`) picks `-clone1`, matching
/// real podman's own checked-directly `CheckName` collision-avoidance
/// algorithm exactly.
#[test]
fn clone_default_name_avoids_a_collision_with_an_incrementing_suffix() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    seed(storage_dir.path(), "ociman-test/clone-collision:latest");

    let create = ociman(
        storage_dir.path(),
        &[
            "create",
            "--name",
            "source",
            "ociman-test/clone-collision:latest",
            "true",
        ],
    );
    assert!(create.status.success(), "{create:?}");

    let clone1 = ociman(storage_dir.path(), &["container", "clone", "source"]);
    assert!(clone1.status.success(), "{clone1:?}");
    let clone1_id = String::from_utf8_lossy(&clone1.stdout).trim().to_string();

    let clone2 = ociman(storage_dir.path(), &["container", "clone", "source"]);
    assert!(clone2.status.success(), "{clone2:?}");
    let clone2_id = String::from_utf8_lossy(&clone2.stdout).trim().to_string();
    assert_ne!(clone1_id, clone2_id);

    let first = ociman(storage_dir.path(), &["inspect", "source-clone", "--json"]);
    assert!(first.status.success());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&first.stdout).unwrap()["id"],
        clone1_id
    );

    let second = ociman(storage_dir.path(), &["inspect", "source-clone1", "--json"]);
    assert!(second.status.success(), "{second:?}");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&second.stdout).unwrap()["id"],
        clone2_id
    );
}

/// An explicit `NAME` positional wins outright, matching real `podman
/// container clone CONTAINER NAME` exactly.
#[test]
fn clone_accepts_an_explicit_name() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    seed(storage_dir.path(), "ociman-test/clone-name:latest");
    let create = ociman(
        storage_dir.path(),
        &["create", "ociman-test/clone-name:latest", "true"],
    );
    assert!(create.status.success(), "{create:?}");
    let source_id = String::from_utf8_lossy(&create.stdout).trim().to_string();

    let clone = ociman(
        storage_dir.path(),
        &["container", "clone", &source_id, "my-explicit-clone"],
    );
    assert!(
        clone.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&clone.stderr)
    );
    let clone_id = String::from_utf8_lossy(&clone.stdout).trim().to_string();

    let by_name = ociman(
        storage_dir.path(),
        &["inspect", "my-explicit-clone", "--json"],
    );
    assert!(by_name.status.success(), "{by_name:?}");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&by_name.stdout).unwrap()["id"],
        clone_id
    );
}

/// An explicit `NAME` already in use by another container is a real,
/// immediate error.
#[test]
fn clone_with_an_already_used_explicit_name_is_a_clear_error() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    seed(storage_dir.path(), "ociman-test/clone-name-taken:latest");
    ociman(
        storage_dir.path(),
        &[
            "create",
            "--name",
            "source",
            "ociman-test/clone-name-taken:latest",
            "true",
        ],
    );
    ociman(
        storage_dir.path(),
        &[
            "create",
            "--name",
            "taken",
            "ociman-test/clone-name-taken:latest",
            "true",
        ],
    );

    let clone = ociman(
        storage_dir.path(),
        &["container", "clone", "source", "taken"],
    );
    assert!(!clone.status.success());
    assert!(
        String::from_utf8_lossy(&clone.stderr).contains("already in use"),
        "{}",
        String::from_utf8_lossy(&clone.stderr)
    );
}

/// `--destroy` removes the source after a successful clone; the
/// source must already be genuinely *stopped* (matching `ociman rm`'s
/// own already-established rule: a merely `created`, never-started
/// container needs `--force` too, exactly like a running one) unless
/// `--force` is also given.
#[test]
fn clone_destroy_removes_the_source_after_success() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    seed(storage_dir.path(), "ociman-test/clone-destroy:latest");
    // Genuinely *stopped* (ran to completion), not merely `created` --
    // see this test's own doc comment for why that distinction
    // matters here.
    let run = ociman(
        storage_dir.path(),
        &["run", "ociman-test/clone-destroy:latest", "true"],
    );
    assert!(run.status.success(), "{run:?}");
    let source_id = {
        let ps = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
        String::from_utf8_lossy(&ps.stdout).trim().to_string()
    };
    assert!(!source_id.is_empty());
    assert_eq!(
        inspect_json(storage_dir.path(), &source_id)["status"],
        "stopped"
    );

    let clone = ociman(
        storage_dir.path(),
        &["container", "clone", "--destroy", &source_id],
    );
    assert!(
        clone.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&clone.stderr)
    );
    let clone_id = String::from_utf8_lossy(&clone.stdout).trim().to_string();

    // The source no longer exists; the clone does.
    let source_inspect = ociman(storage_dir.path(), &["inspect", &source_id]);
    assert!(!source_inspect.status.success());
    assert_eq!(
        inspect_json(storage_dir.path(), &clone_id)["status"],
        "created"
    );
}

/// `--force` without `--destroy` is a real, immediate error, matching
/// real podman's own exact wording.
#[test]
fn clone_force_without_destroy_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let clone = ociman(
        storage_dir.path(),
        &["container", "clone", "--force", "does-not-matter"],
    );
    assert!(!clone.status.success());
    assert!(
        String::from_utf8_lossy(&clone.stderr).contains("cannot set --force without --destroy"),
        "{}",
        String::from_utf8_lossy(&clone.stderr)
    );
}

/// `--destroy` alone refuses to remove a still-*running* source
/// (matching this project's own already-established `rm` rule); with
/// `--force` too, it forcefully removes it anyway.
#[test]
fn clone_destroy_on_a_running_source_needs_force() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    seed(
        storage_dir.path(),
        "ociman-test/clone-destroy-running:latest",
    );

    let run = Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_dir.path())
        .env_remove("OCI_TOOLS_LOG")
        .args([
            "run",
            "-d",
            "--name",
            "source",
            "ociman-test/clone-destroy-running:latest",
            "sleep",
            "30",
        ])
        .stdin(Stdio::null())
        .output()
        .expect("failed to spawn ociman run -d");
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let source_id = {
        let ps = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
        String::from_utf8_lossy(&ps.stdout).trim().to_string()
    };
    assert!(!source_id.is_empty());
    assert_eq!(
        wait_for_status(
            storage_dir.path(),
            &source_id,
            "running",
            Duration::from_secs(20)
        ),
        "running"
    );

    let clone_no_force = ociman(
        storage_dir.path(),
        &["container", "clone", "--destroy", "source"],
    );
    assert!(!clone_no_force.status.success());
    assert!(
        String::from_utf8_lossy(&clone_no_force.stderr).contains("not stopped"),
        "{}",
        String::from_utf8_lossy(&clone_no_force.stderr)
    );
    // The source is untouched, but a first clone was still created --
    // give it its own distinct name so the next attempt below doesn't
    // collide.
    assert_eq!(
        inspect_json(storage_dir.path(), &source_id)["status"],
        "running"
    );

    let clone_force = ociman(
        storage_dir.path(),
        &[
            "container",
            "clone",
            "--destroy",
            "--force",
            "source",
            "forced-clone",
        ],
    );
    assert!(
        clone_force.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&clone_force.stderr)
    );
    let inspect_source = ociman(storage_dir.path(), &["inspect", &source_id]);
    assert!(!inspect_source.status.success());
}

/// `--run` starts the clone (detached) immediately after creating it.
#[test]
fn clone_run_starts_the_clone_detached() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    seed(storage_dir.path(), "ociman-test/clone-run:latest");
    let create = ociman(
        storage_dir.path(),
        &[
            "create",
            "ociman-test/clone-run:latest",
            "sh",
            "-c",
            "sleep 30",
        ],
    );
    assert!(create.status.success(), "{create:?}");
    let source_id = String::from_utf8_lossy(&create.stdout).trim().to_string();

    let clone = ociman(
        storage_dir.path(),
        &["container", "clone", "--run", &source_id],
    );
    assert!(
        clone.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&clone.stderr)
    );
    // Exactly one line -- a real, previously-hit double-print bug
    // (`cmd_start`'s own reused path already prints the new id once
    // itself; this command's own trailing print used to run
    // unconditionally too, printing it twice) caught by this exact
    // assertion before landing.
    let stdout = String::from_utf8_lossy(&clone.stdout);
    assert_eq!(stdout.lines().count(), 1, "stdout: {stdout:?}");
    let clone_id = stdout.trim().to_string();

    assert_eq!(
        wait_for_status(
            storage_dir.path(),
            &clone_id,
            "running",
            Duration::from_secs(20)
        ),
        "running"
    );
    // The source is untouched, still merely `created`.
    assert_eq!(
        inspect_json(storage_dir.path(), &source_id)["status"],
        "created"
    );

    let _ = ociman(storage_dir.path(), &["kill", &clone_id]);
}

/// Cloning an unknown container is a real, immediate error.
#[test]
fn clone_of_an_unknown_container_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let clone = ociman(
        storage_dir.path(),
        &["container", "clone", "does-not-exist"],
    );
    assert!(!clone.status.success());
    assert!(
        String::from_utf8_lossy(&clone.stderr).contains("does not exist"),
        "{}",
        String::from_utf8_lossy(&clone.stderr)
    );
}
