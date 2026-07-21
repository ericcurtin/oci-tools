//! `ociman prune` integration tests: reclaiming disk space no longer
//! needed by anything currently tagged — real blob garbage collection
//! (`oci_store::Store::gc`, implemented and unit-tested since
//! milestone 2 but never wired to any command before this one) and
//! rootfs-cache pruning (`docs/design/0109`/`0111`) together. Same
//! fully offline seeded-image approach `ociman_run.rs`/`ociman_rmi.rs`
//! established.

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
fn prune_on_an_empty_store_reports_nothing_to_reclaim() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let prune = ociman(storage_dir.path(), &["--json", "prune"]);
    assert!(
        prune.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&prune.stderr)
    );
    let view: serde_json::Value = serde_json::from_slice(&prune.stdout).unwrap();
    assert_eq!(view["blobs_removed"], 0);
    assert_eq!(view["blobs_reclaimed_bytes"], 0);
    assert_eq!(view["rootfs_cache_entries_removed"], 0);
    assert_eq!(view["rootfs_cache_reclaimed_bytes"], 0);
}

#[test]
fn prune_removes_a_blob_no_image_references_anymore() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/prune-orphan:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let record = store
        .resolve_image("docker.io/ociman-test/prune-orphan:latest")
        .unwrap()
        .unwrap();
    let manifest = store.image_manifest(&record).unwrap();
    let layer_digest = manifest.layers[0].digest.clone();
    assert!(store.has_blob(&layer_digest));

    // Untag it -- `rmi` only ever removes the pointer (see
    // `docs/design/0102`), so the blob itself is still on disk,
    // unreferenced, until a real `prune` run reclaims it.
    let rmi = ociman(
        storage_dir.path(),
        &["rmi", "ociman-test/prune-orphan:latest"],
    );
    assert!(
        rmi.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&rmi.stderr)
    );
    assert!(
        store.has_blob(&layer_digest),
        "rmi alone must not have already removed the blob"
    );

    let prune = ociman(storage_dir.path(), &["--json", "prune"]);
    assert!(
        prune.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&prune.stderr)
    );
    let view: serde_json::Value = serde_json::from_slice(&prune.stdout).unwrap();
    assert!(view["blobs_removed"].as_u64().unwrap() > 0, "{view:?}");
    assert!(
        view["blobs_reclaimed_bytes"].as_u64().unwrap() > 0,
        "{view:?}"
    );
    assert!(!store.has_blob(&layer_digest));
}

#[test]
fn prune_keeps_a_blob_a_real_tag_still_references() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/prune-kept:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let record = store
        .resolve_image("docker.io/ociman-test/prune-kept:latest")
        .unwrap()
        .unwrap();
    let manifest = store.image_manifest(&record).unwrap();
    let layer_digest = manifest.layers[0].digest.clone();

    let prune = ociman(storage_dir.path(), &["--json", "prune"]);
    assert!(prune.status.success());
    let view: serde_json::Value = serde_json::from_slice(&prune.stdout).unwrap();
    assert_eq!(view["blobs_removed"], 0, "{view:?}");
    assert!(store.has_blob(&layer_digest));
    assert!(
        store
            .resolve_image("docker.io/ociman-test/prune-kept:latest")
            .unwrap()
            .is_some()
    );
}

/// Whether a real `ociman run` of this image actually populated a
/// rootfs-cache entry at all depends on this host's own real
/// rootless-overlay support (`docs/design/0108`/`0110`) -- an
/// environment without it takes the always-correct extraction
/// fallback instead, which never touches the cache. Both are real,
/// valid outcomes; this test only asserts the *if it exists, prune
/// removes it once orphaned* half, which holds either way, rather
/// than asserting the cache is unconditionally populated (which would
/// make this test depend on this specific host's own overlay support
/// to pass at all).
#[test]
fn prune_removes_an_orphaned_rootfs_cache_entry_when_one_exists() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/prune-cache:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "true".to_string(),
            ]),
            ..Default::default()
        },
    );
    let record = store
        .resolve_image("docker.io/ociman-test/prune-cache:latest")
        .unwrap()
        .unwrap();

    let run = ociman(
        storage_dir.path(),
        &["run", "--rm", "ociman-test/prune-cache:latest"],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    let cache_dir = storage_dir
        .path()
        .join("rootfs-cache")
        .join(record.manifest_digest.hex());
    if !cache_dir.exists() {
        eprintln!(
            "skipping the rest of this test: this host's own rootless-overlay support (or \
             lack of it) meant `ociman run` never populated a rootfs-cache entry at all"
        );
        return;
    }

    let rmi = ociman(
        storage_dir.path(),
        &["rmi", "ociman-test/prune-cache:latest"],
    );
    assert!(rmi.status.success());
    assert!(
        cache_dir.exists(),
        "rmi alone must not have already removed the rootfs-cache entry"
    );

    let prune = ociman(storage_dir.path(), &["--json", "prune"]);
    assert!(
        prune.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&prune.stderr)
    );
    let view: serde_json::Value = serde_json::from_slice(&prune.stdout).unwrap();
    assert!(
        view["rootfs_cache_entries_removed"].as_u64().unwrap() > 0,
        "{view:?}"
    );
    assert!(!cache_dir.exists());
}
