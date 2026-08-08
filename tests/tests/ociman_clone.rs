//! `ociman container clone` integration tests (`docs/design/0474`,
//! `0571`): this project's own first, deliberately narrower slice of
//! real `podman container clone` — clones from the exact same image
//! the source container itself already used unless a positional
//! `IMAGE` is given (`0571`), in which case the clone's own fresh
//! rootfs is extracted from *that* image instead while every other
//! part of the config (command, env, labels, ...) still comes from
//! the source's own current `config.json` unchanged, matching real
//! podman's own checked-directly `ConfigToSpec` behavior exactly. The
//! clone is always left `Created` unless `--run` says otherwise.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use oci_spec_types::image::ContainerConfig;
use oci_store::Store;

use oci_tools_tests::{bin_path, busybox_path, seed_image, seed_image_with_files};

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

/// A positional `IMAGE` (`docs/design/0571`) extracts the clone's own
/// fresh rootfs from a genuinely *different* image than the source's
/// own recorded one -- proven with two distinguishably-seeded images
/// (each with its own marker file) -- while every other part of the
/// config still comes from the source container's own current
/// `config.json` unchanged: the clone's own `command` field matches
/// the source's explicit command, not the new image's own default
/// `cmd`, matching real podman's own checked-directly `ConfigToSpec`
/// behavior exactly (see `Command::Clone`'s own doc comment).
#[test]
fn clone_with_a_different_image_extracts_its_rootfs_but_keeps_the_sources_own_config() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image_with_files(
        &store,
        "ociman-test/clone-image-a:latest",
        &busybox,
        &["sh", "true"],
        &[("marker-a.txt", b"this is image a")],
        ContainerConfig {
            cmd: Some(vec!["true".to_string()]),
            ..Default::default()
        },
    );
    seed_image_with_files(
        &store,
        "ociman-test/clone-image-b:latest",
        &busybox,
        &["sh", "true"],
        &[("marker-b.txt", b"this is image b")],
        ContainerConfig {
            cmd: Some(vec![
                "sh".to_string(),
                "-c".to_string(),
                "exit 42".to_string(),
            ]),
            ..Default::default()
        },
    );

    let create = ociman(
        storage_dir.path(),
        &[
            "create",
            "--name",
            "clone-image-source",
            "ociman-test/clone-image-a:latest",
            "sh",
            "-c",
            "echo source-cmd-marker",
        ],
    );
    assert!(create.status.success(), "{create:?}");
    let source_id = String::from_utf8_lossy(&create.stdout).trim().to_string();

    let clone = ociman(
        storage_dir.path(),
        &[
            "container",
            "clone",
            "clone-image-source",
            "clone-image-dest",
            "ociman-test/clone-image-b:latest",
        ],
    );
    assert!(
        clone.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&clone.stderr)
    );
    let clone_id = String::from_utf8_lossy(&clone.stdout).trim().to_string();

    // The clone's own recorded image is the new one, not the
    // source's -- normalized the same way a real `ociman pull` would
    // leave it (`seed_image`'s own doc comment).
    let clone_json = inspect_json(storage_dir.path(), &clone_id);
    assert_eq!(
        clone_json["image"],
        "docker.io/ociman-test/clone-image-b:latest"
    );
    assert_eq!(
        inspect_json(storage_dir.path(), &source_id)["image"],
        "docker.io/ociman-test/clone-image-a:latest"
    );

    // The clone's own command is still the source's explicit one, not
    // image b's own default `cmd` -- proving the config, not just the
    // id string, was faithfully copied rather than re-derived from
    // the new image.
    assert_eq!(clone_json["command"], "sh -c echo source-cmd-marker");

    // The clone's own real rootfs genuinely has image b's marker
    // file, not image a's -- proving the rootfs extraction itself
    // really did come from the new image, not just its name being
    // recorded.
    let rootfs = clone_json["rootfs"].as_str().unwrap();
    assert!(Path::new(rootfs).join("marker-b.txt").exists());
    assert!(!Path::new(rootfs).join("marker-a.txt").exists());

    // Starting the clone and attaching runs the source's own command
    // for real, inside image b's own real rootfs.
    let start = ociman(storage_dir.path(), &["start", "--attach", &clone_id]);
    assert!(start.status.success(), "{start:?}");
    assert_eq!(
        String::from_utf8_lossy(&start.stdout).trim(),
        "source-cmd-marker"
    );
}

/// Real podman's own exact positional-consumption rule: a *second*
/// positional (no third) is always the new name, never an image --
/// even if it happens to look like one. Matching this via clap's own
/// identical left-to-right optional-positional consumption, not a
/// special case of this project's own.
#[test]
fn clone_with_only_two_positionals_treats_the_second_as_a_name_never_an_image() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    seed(
        storage_dir.path(),
        "ociman-test/clone-two-positional:latest",
    );

    let create = ociman(
        storage_dir.path(),
        &[
            "create",
            "--name",
            "two-positional-source",
            "ociman-test/clone-two-positional:latest",
            "true",
        ],
    );
    assert!(create.status.success(), "{create:?}");
    let source_id = String::from_utf8_lossy(&create.stdout).trim().to_string();

    let clone = ociman(
        storage_dir.path(),
        &[
            "container",
            "clone",
            "two-positional-source",
            "alpine-1.0.0",
        ],
    );
    assert!(
        clone.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&clone.stderr)
    );
    let clone_id = String::from_utf8_lossy(&clone.stdout).trim().to_string();

    // The "tag-shaped" second positional became the new container's
    // own literal name, and the clone still used the source's own
    // recorded image -- exactly like real podman's own `case 2`.
    let by_name = ociman(storage_dir.path(), &["inspect", "alpine-1.0.0", "--json"]);
    assert!(by_name.status.success(), "{by_name:?}");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&by_name.stdout).unwrap()["id"],
        clone_id
    );
    let clone_json = inspect_json(storage_dir.path(), &clone_id);
    assert_eq!(
        clone_json["image"],
        "docker.io/ociman-test/clone-two-positional:latest"
    );
    assert_eq!(
        inspect_json(storage_dir.path(), &source_id)["image"],
        clone_json["image"]
    );
}

/// A positional `IMAGE` that isn't already stored locally attempts a
/// real pull (`--pull missing`, this project's own default policy,
/// matching real podman's own identical default) -- proven here
/// against an address nothing is listening on (the same
/// `UNREACHABLE_HOST` pattern `ociman_pull_policy.rs`'s own tests
/// already establish, avoiding any real network dependency), which is
/// a real, immediate error, never a silent fallback to the source's
/// own image.
#[test]
fn clone_with_an_unresolvable_image_is_a_clear_error() {
    let Some(_busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    seed(storage_dir.path(), "ociman-test/clone-bad-image:latest");

    let create = ociman(
        storage_dir.path(),
        &[
            "create",
            "--name",
            "bad-image-source",
            "ociman-test/clone-bad-image:latest",
            "true",
        ],
    );
    assert!(create.status.success(), "{create:?}");

    let clone = ociman(
        storage_dir.path(),
        &[
            "container",
            "clone",
            "bad-image-source",
            "bad-image-dest",
            "127.0.0.1:1/testrepo:latest",
        ],
    );
    assert!(!clone.status.success());
}
