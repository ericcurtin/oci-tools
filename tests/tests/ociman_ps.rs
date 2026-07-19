//! `ociman ps`/`rm`/`run --rm` integration tests: the persistent
//! container tracking `ociman run` (0020) gained on top of its
//! previously ephemeral-only model (`docs/design/0021`). Same fully
//! offline approach as `ociman_run.rs` (a synthetic-but-structurally-
//! real seeded image, no registry access needed).

use std::path::Path;
use std::process::Command;

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

#[test]
fn run_persists_a_container_ps_and_rm_can_see_and_remove() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/ps-basic:latest",
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

    // No containers at all before `run`.
    let ps_before = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    assert!(ps_before.status.success());
    assert!(String::from_utf8_lossy(&ps_before.stdout).trim().is_empty());

    let run = ociman(storage_dir.path(), &["run", "ociman-test/ps-basic:latest"]);
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    // `ps` (running only) shows nothing: the container already exited
    // by the time the foreground `run` above returned.
    let ps_running_only = ociman(storage_dir.path(), &["ps", "-q"]);
    assert!(ps_running_only.status.success());
    assert!(
        String::from_utf8_lossy(&ps_running_only.stdout)
            .trim()
            .is_empty()
    );

    // `ps -a` shows the stopped container.
    let ps_all = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    assert!(ps_all.status.success());
    let id = String::from_utf8_lossy(&ps_all.stdout).trim().to_string();
    assert!(!id.is_empty(), "expected exactly one container id");

    let ps_json = ociman(storage_dir.path(), &["ps", "-a", "--json"]);
    assert!(ps_json.status.success());
    let views: serde_json::Value = serde_json::from_slice(&ps_json.stdout).unwrap();
    let entry = &views[0];
    assert_eq!(entry["id"], id);
    assert_eq!(entry["image"], "docker.io/ociman-test/ps-basic:latest");
    assert_eq!(entry["status"], "stopped");
    assert_eq!(entry["exit_code"], 0);

    // `rm` removes it; `ps -a` is empty again afterward.
    let rm = ociman(storage_dir.path(), &["rm", &id]);
    assert!(
        rm.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rm.stderr)
    );
    let ps_after_rm = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    assert!(
        String::from_utf8_lossy(&ps_after_rm.stdout)
            .trim()
            .is_empty()
    );
}

#[test]
fn run_rm_flag_removes_the_container_automatically() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/auto-rm:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "exit 3".to_string(),
            ]),
            ..Default::default()
        },
    );

    let run = ociman(
        storage_dir.path(),
        &["run", "--rm", "ociman-test/auto-rm:latest"],
    );
    assert_eq!(run.status.code(), Some(3));

    let ps_all = ociman(storage_dir.path(), &["ps", "-a", "-q"]);
    assert!(
        String::from_utf8_lossy(&ps_all.stdout).trim().is_empty(),
        "expected --rm to remove the container's record"
    );
}

#[test]
fn rm_without_force_refuses_to_remove_a_container_still_marked_running() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/refuse-rm:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    // Seed a bare "created" (never-run) record directly via the same
    // state store `ociman` itself would open, rather than running a
    // real long-lived container — this test only needs a record whose
    // `effective_status` isn't `Stopped` yet, and a `create`d-but-
    // never-`run` one is the simplest way to get exactly that.
    let containers_root = storage_dir.path().join("containers");
    let containers = oci_runtime_core::StateStore::open(&containers_root).unwrap();
    containers
        .create(
            "still-creating",
            Path::new("/bundle"),
            Path::new("/bundle/rootfs"),
            Default::default(),
        )
        .unwrap();

    let refused = ociman(storage_dir.path(), &["rm", "still-creating"]);
    assert!(
        !refused.status.success(),
        "rm without --force should refuse a non-stopped container"
    );

    let forced = ociman(storage_dir.path(), &["rm", "--force", "still-creating"]);
    assert!(
        forced.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&forced.stderr)
    );
}

#[test]
fn rm_of_a_nonexistent_container_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(storage_dir.path(), &["rm", "does-not-exist"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("does not exist"));
}
