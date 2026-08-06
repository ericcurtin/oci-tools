//! `ociman image prune` integration tests (`docs/design/0359`): real
//! `podman image prune`'s own narrower equivalent of `ociman prune`
//! (see `ociman_prune.rs`) — the same image removal, blob GC, and
//! rootfs-cache GC passes, but never touching any container at all
//! (checked directly against a real installed `podman image prune`),
//! unlike `ociman prune`/real `podman system prune`.

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
fn image_prune_on_an_empty_store_reports_nothing_to_reclaim() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let prune = ociman(storage_dir.path(), &["--json", "image", "prune"]);
    assert!(
        prune.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&prune.stderr)
    );
    let view: serde_json::Value = serde_json::from_slice(&prune.stdout).unwrap();
    assert_eq!(view["images_removed"], serde_json::json!([]));
    assert_eq!(view["blobs_removed"], 0);
    assert_eq!(view["blobs_reclaimed_bytes"], 0);
    assert_eq!(view["rootfs_cache_entries_removed"], 0);
    assert_eq!(view["rootfs_cache_reclaimed_bytes"], 0);
    // Never present at all -- unlike `ociman prune`'s own JSON shape,
    // this narrower command has no concept of either field.
    assert!(view.get("containers_removed").is_none(), "{view:?}");
    assert!(
        view.get("build_scratch_entries_removed").is_none(),
        "{view:?}"
    );
}

/// Without `--all`, a still-tagged-but-unused image is left alone,
/// matching `ociman prune`'s own identical default (and a real
/// installed `podman image prune`'s own identical default).
#[test]
fn image_prune_without_all_leaves_an_unused_but_still_tagged_image_alone() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/image-prune-default:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let prune = ociman(storage_dir.path(), &["--json", "image", "prune"]);
    assert!(prune.status.success(), "{prune:?}");
    let view: serde_json::Value = serde_json::from_slice(&prune.stdout).unwrap();
    assert_eq!(view["images_removed"], serde_json::json!([]), "{view:?}");
    assert!(
        store
            .resolve_image("docker.io/ociman-test/image-prune-default:latest")
            .unwrap()
            .is_some()
    );
}

/// `--all` reaches a still-tagged image nothing currently uses,
/// matching `ociman prune --all`'s own identical `--all` semantics.
#[test]
fn image_prune_all_removes_an_unused_tagged_image() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/image-prune-all:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let prune = ociman(storage_dir.path(), &["--json", "image", "prune", "--all"]);
    assert!(prune.status.success(), "{prune:?}");
    let view: serde_json::Value = serde_json::from_slice(&prune.stdout).unwrap();
    assert_eq!(
        view["images_removed"],
        serde_json::json!(["docker.io/ociman-test/image-prune-all:latest"]),
        "{view:?}"
    );
    assert!(
        store
            .resolve_image("docker.io/ociman-test/image-prune-all:latest")
            .unwrap()
            .is_none()
    );
}

/// `image prune --force`/`-f` (0521): accepted for real CLI
/// compatibility, but changes nothing -- the identical "nothing to
/// skip" reasoning `container prune --force` already established.
#[test]
fn image_prune_force_flag_is_accepted_and_behaves_identically() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/image-prune-force:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let prune = ociman(
        storage_dir.path(),
        &["--json", "image", "prune", "--all", "--force"],
    );
    assert!(prune.status.success(), "{prune:?}");
    let view: serde_json::Value = serde_json::from_slice(&prune.stdout).unwrap();
    assert_eq!(
        view["images_removed"],
        serde_json::json!(["docker.io/ociman-test/image-prune-force:latest"]),
        "{view:?}"
    );
    assert!(
        store
            .resolve_image("docker.io/ociman-test/image-prune-force:latest")
            .unwrap()
            .is_none()
    );
}

/// Unlike `ociman prune`, `ociman image prune` never removes a real,
/// stopped container -- even `--all` given -- matching a real
/// installed `podman image prune`'s own identical, checked-directly
/// scope exactly (only `podman container prune`/`podman system prune`
/// ever touch a container at all).
#[test]
fn image_prune_never_removes_a_stopped_container() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/image-prune-container:latest",
        &busybox,
        &["sh", "true"],
        ContainerConfig::default(),
    );
    let run = ociman(
        storage_dir.path(),
        &["run", "ociman-test/image-prune-container:latest", "true"],
    );
    assert!(run.status.success(), "{run:?}");
    let container_id =
        String::from_utf8_lossy(&ociman(storage_dir.path(), &["ps", "-a", "-q"]).stdout)
            .trim()
            .to_string();
    assert!(!container_id.is_empty());

    // Even with `--all`, the container survives -- and because it
    // does, its own image is still correctly protected too (this
    // command never prunes containers first the way `ociman prune`
    // does).
    let prune = ociman(storage_dir.path(), &["--json", "image", "prune", "--all"]);
    assert!(prune.status.success(), "{prune:?}");
    let view: serde_json::Value = serde_json::from_slice(&prune.stdout).unwrap();
    assert_eq!(view["images_removed"], serde_json::json!([]), "{view:?}");
    assert_eq!(
        String::from_utf8_lossy(&ociman(storage_dir.path(), &["ps", "-a", "-q"]).stdout).trim(),
        container_id
    );
    assert!(
        store
            .resolve_image("docker.io/ociman-test/image-prune-container:latest")
            .unwrap()
            .is_some()
    );
}

/// `--filter dangling=false` (no `--all` at all) removes every unused
/// image regardless of tag, the exact same real, checked-directly
/// override `ociman prune --filter dangling=false` already has
/// (`docs/design/0181`) — same filter engine, same override rule,
/// just reused by this narrower command.
#[test]
fn image_prune_filter_dangling_false_removes_a_tagged_image_without_all() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/image-prune-filter-dangling:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let prune = ociman(
        storage_dir.path(),
        &["--json", "image", "prune", "--filter", "dangling=false"],
    );
    assert!(prune.status.success(), "{prune:?}");
    let view: serde_json::Value = serde_json::from_slice(&prune.stdout).unwrap();
    assert_eq!(
        view["images_removed"],
        serde_json::json!(["docker.io/ociman-test/image-prune-filter-dangling:latest"]),
        "{view:?}"
    );
    assert!(
        store
            .resolve_image("docker.io/ociman-test/image-prune-filter-dangling:latest")
            .unwrap()
            .is_none()
    );
}
