//! `ociman rename` integration tests: rewrite a container's own
//! `--name` annotation — matching real `docker rename`/`podman
//! rename` exactly (`~/git/podman/cmd/podman/containers/rename.go`:
//! silent on success). Same fully offline seeded-image approach
//! `ociman_run.rs`/`ociman_name.rs` established.

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

fn container_names(storage_root: &Path) -> Vec<(String, String)> {
    let out = ociman(storage_root, &["ps", "-a", "--json"]);
    assert!(out.status.success());
    let views: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    views
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["id"].as_str().unwrap().to_string(),
                e["name"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

#[test]
fn rename_changes_the_containers_own_name_and_it_becomes_usable_immediately() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/rename-basic:latest",
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
            "rename-old",
            "ociman-test/rename-basic:latest",
        ],
    );
    assert!(run.status.success());

    let rename = ociman(storage_dir.path(), &["rename", "rename-old", "rename-new"]);
    assert!(
        rename.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rename.stderr)
    );
    // Real `docker`/`podman rename` print nothing at all on success.
    assert!(rename.stdout.is_empty());

    let names = container_names(storage_dir.path());
    assert_eq!(names.len(), 1);
    assert_eq!(names[0].1, "rename-new");

    // The new name is immediately usable wherever the old one was --
    // e.g. `rm`.
    let rm = ociman(storage_dir.path(), &["rm", "rename-new"]);
    assert!(
        rm.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rm.stderr)
    );
}

#[test]
fn rename_can_target_a_container_by_its_real_id_too() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/rename-by-id:latest",
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
        &["run", "ociman-test/rename-by-id:latest"],
    );
    assert!(run.status.success());
    let id = container_names(storage_dir.path())[0].0.clone();

    let rename = ociman(storage_dir.path(), &["rename", &id, "by-id-new-name"]);
    assert!(rename.status.success());

    let names = container_names(storage_dir.path());
    assert_eq!(names[0].1, "by-id-new-name");
}

#[test]
fn renaming_a_container_to_its_own_current_name_is_a_harmless_noop() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/rename-noop:latest",
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
            "same-name",
            "ociman-test/rename-noop:latest",
        ],
    );
    assert!(run.status.success());

    let rename = ociman(storage_dir.path(), &["rename", "same-name", "same-name"]);
    assert!(
        rename.status.success(),
        "renaming a container to its own current name should be a harmless no-op: {}",
        String::from_utf8_lossy(&rename.stderr)
    );
}

#[test]
fn rename_refuses_a_name_already_in_use_by_a_different_container() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/rename-collision:latest",
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

    let run1 = ociman(
        storage_dir.path(),
        &[
            "run",
            "--name",
            "first-container",
            "ociman-test/rename-collision:latest",
        ],
    );
    assert!(run1.status.success());
    let run2 = ociman(
        storage_dir.path(),
        &[
            "run",
            "--name",
            "second-container",
            "ociman-test/rename-collision:latest",
        ],
    );
    assert!(run2.status.success());

    let rename = ociman(
        storage_dir.path(),
        &["rename", "second-container", "first-container"],
    );
    assert!(
        !rename.status.success(),
        "renaming to a name already used by a different container should fail"
    );
}

#[test]
fn rename_rejects_an_invalid_new_name() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/rename-invalid:latest",
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
        &["run", "ociman-test/rename-invalid:latest"],
    );
    assert!(run.status.success());
    let id = container_names(storage_dir.path())[0].0.clone();

    let rename = ociman(storage_dir.path(), &["rename", &id, "bad name!"]);
    assert!(!rename.status.success());
}

#[test]
fn rename_of_a_nonexistent_container_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(storage_dir.path(), &["rename", "does-not-exist", "foo"]);
    assert!(!out.status.success());
}
