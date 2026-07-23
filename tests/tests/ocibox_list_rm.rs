//! `ocibox list`/`ocibox rm` integration tests: exercises the actual
//! built `ocibox` binary — `ocibox create`'s own tests
//! (`ocibox_create.rs`) already cover image resolution and rootfs
//! extraction directly; this covers the rest of the family that makes
//! `create` actually manageable.

use std::path::Path;
use std::process::Command;

use oci_spec_types::image::ContainerConfig;
use oci_store::Store;

use oci_tools_tests::{bin_path, busybox_path, seed_image};

fn ocibox(storage_root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin_path("ocibox"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
        .env_remove("OCI_TOOLS_LOG")
        .args(args)
        .output()
        .expect("failed to spawn ocibox")
}

#[test]
fn list_on_an_empty_store_says_so_and_exits_success() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let list = ocibox(storage_dir.path(), &["list"]);
    assert!(
        list.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&list.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&list.stdout).trim(), "no boxes");
}

#[test]
fn list_shows_every_created_box_sorted_by_name() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ocibox-test/list-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    for name in ["zeta", "alpha", "mid"] {
        let create = ocibox(
            storage_dir.path(),
            &[
                "create",
                "--image",
                "ocibox-test/list-base:latest",
                "--name",
                name,
            ],
        );
        assert!(create.status.success());
    }

    let list = ocibox(storage_dir.path(), &["list"]);
    assert!(list.status.success());
    let stdout = String::from_utf8_lossy(&list.stdout);
    let names: Vec<&str> = stdout
        .lines()
        .skip(1) // header
        .map(|line| line.split_whitespace().next().unwrap())
        .collect();
    assert_eq!(
        names,
        vec!["alpha", "mid", "zeta"],
        "boxes should be sorted by name, not creation order: {stdout:?}"
    );
}

#[test]
fn list_json_reports_every_field_of_the_persisted_record() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ocibox-test/list-json-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let create = ocibox(
        storage_dir.path(),
        &[
            "create",
            "--image",
            "ocibox-test/list-json-base:latest",
            "--name",
            "jsonbox",
        ],
    );
    assert!(create.status.success());

    let list = ocibox(storage_dir.path(), &["--json", "list"]);
    assert!(list.status.success());
    let view: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    let boxes = view.as_array().unwrap();
    assert_eq!(boxes.len(), 1);
    assert_eq!(boxes[0]["name"], "jsonbox");
    assert_eq!(
        boxes[0]["image"],
        "docker.io/ocibox-test/list-json-base:latest"
    );
    assert!(
        boxes[0]["manifest_digest"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert!(boxes[0]["created"].as_str().is_some());
}

/// `ls` is a real alias for `list`, matching real `distrobox list`'s
/// own identical alias.
#[test]
fn ls_is_an_alias_for_list() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let ls = ocibox(storage_dir.path(), &["ls"]);
    assert!(ls.status.success());
    assert_eq!(String::from_utf8_lossy(&ls.stdout).trim(), "no boxes");
}

#[test]
fn rm_removes_a_real_box_entirely() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ocibox-test/rm-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let create = ocibox(
        storage_dir.path(),
        &[
            "create",
            "--image",
            "ocibox-test/rm-base:latest",
            "--name",
            "rmbox",
        ],
    );
    assert!(create.status.success());
    let box_dir = storage_dir.path().join("boxes").join("rmbox");
    assert!(box_dir.is_dir());

    let rm = ocibox(storage_dir.path(), &["rm", "rmbox"]);
    assert!(
        rm.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rm.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&rm.stdout).trim(), "rmbox");
    assert!(!box_dir.exists(), "the whole box directory should be gone");

    let list = ocibox(storage_dir.path(), &["list"]);
    assert_eq!(String::from_utf8_lossy(&list.stdout).trim(), "no boxes");
}

#[test]
fn rm_of_an_unknown_name_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let rm = ocibox(storage_dir.path(), &["rm", "doesnotexist"]);
    assert!(!rm.status.success());
    assert!(
        String::from_utf8_lossy(&rm.stderr).contains("no such box"),
        "{}",
        String::from_utf8_lossy(&rm.stderr)
    );
}

/// A real, checked-directly security concern: `rm`'s own `name`
/// argument must never be usable to escape `boxes_root` via `/`/`..`
/// components -- confirmed directly that a path-traversal attempt is
/// rejected as an invalid name outright, long before any real
/// `remove_dir_all` call could ever be reached.
#[test]
fn rm_rejects_a_path_traversal_attempt_in_the_name() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();
    // A real, harmless canary file outside `boxes_root` entirely --
    // if the path-traversal attempt below were ever allowed through,
    // this is what would prove it (it must still exist afterward).
    let canary = storage_dir.path().join("canary.txt");
    std::fs::write(&canary, b"still here").unwrap();

    let rm = ocibox(storage_dir.path(), &["rm", "../canary.txt"]);
    assert!(!rm.status.success());
    assert!(
        String::from_utf8_lossy(&rm.stderr).contains("invalid box name"),
        "{}",
        String::from_utf8_lossy(&rm.stderr)
    );
    assert!(canary.is_file(), "the canary file must survive untouched");
}
