//! `ociman image mount`/`ociman image unmount` integration tests
//! (`docs/design/0519`): a real, separate, non-alias pair of
//! subcommands from the already-existing container `mount`/`unmount`
//! (`0361`/`0511`) — extracting (if not already cached) an image's
//! own real rootfs and printing its cache path, and a real no-op
//! respectively, correcting a mischaracterization of this pair as
//! "cross-concept aliasing" repeated across `0481`/`0482`/`0499`.

use std::path::Path;
use std::process::Command;

use oci_spec_types::Reference;
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
fn image_mount_extracts_and_prints_the_real_cache_path() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/image-mount:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let normalized = Reference::parse("ociman-test/image-mount:latest")
        .unwrap()
        .to_string();
    let record = store.resolve_image(&normalized).unwrap().unwrap();

    let mount = ociman(
        storage_dir.path(),
        &["image", "mount", "ociman-test/image-mount:latest"],
    );
    assert!(
        mount.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&mount.stderr)
    );
    let printed = String::from_utf8_lossy(&mount.stdout).trim().to_string();
    let expected = storage_dir
        .path()
        .join("rootfs-cache")
        .join(record.manifest_digest.hex());
    assert_eq!(std::path::PathBuf::from(&printed), expected, "{mount:?}");
    assert!(
        Path::new(&printed).is_dir(),
        "the printed path should be a real, already-existing directory"
    );
    // A real, already-extracted rootfs: busybox's own `/bin/sh`
    // should genuinely be there.
    assert!(Path::new(&printed).join("bin/sh").exists());
}

#[test]
fn image_mount_by_id_works_too() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/image-mount-id:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let normalized = Reference::parse("ociman-test/image-mount-id:latest")
        .unwrap()
        .to_string();
    let record = store.resolve_image(&normalized).unwrap().unwrap();
    let short_id = &record.manifest_digest.hex()[..12];

    let mount = ociman(storage_dir.path(), &["image", "mount", short_id]);
    assert!(
        mount.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&mount.stderr)
    );
    let printed = String::from_utf8_lossy(&mount.stdout).trim().to_string();
    assert!(Path::new(&printed).is_dir());
}

#[test]
fn image_mount_accepts_multiple_images_in_one_call() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/image-mount-multi-1:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    seed_image(
        &store,
        "ociman-test/image-mount-multi-2:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let mount = ociman(
        storage_dir.path(),
        &[
            "image",
            "mount",
            "ociman-test/image-mount-multi-1:latest",
            "ociman-test/image-mount-multi-2:latest",
        ],
    );
    assert!(
        mount.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&mount.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&mount.stdout)
            .trim()
            .lines()
            .count(),
        2,
        "one path per image: {mount:?}"
    );
}

#[test]
fn image_mount_with_one_unknown_image_among_valid_ones_mounts_nothing() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/image-mount-partial:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let mount = ociman(
        storage_dir.path(),
        &[
            "image",
            "mount",
            "ociman-test/image-mount-partial:latest",
            "ociman-test/does-not-exist:latest",
        ],
    );
    assert!(!mount.status.success());
    assert!(mount.stdout.is_empty(), "{mount:?}");
    assert!(
        !storage_dir.path().join("rootfs-cache").is_dir(),
        "nothing should have been mounted/cached at all"
    );
}

#[test]
fn image_mount_of_an_unknown_image_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let mount = ociman(storage_dir.path(), &["image", "mount", "never-pulled"]);
    assert!(!mount.status.success());
    assert!(
        String::from_utf8_lossy(&mount.stderr).contains("no such image"),
        "{}",
        String::from_utf8_lossy(&mount.stderr)
    );
}

#[test]
fn image_mount_with_no_image_at_all_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let mount = ociman(storage_dir.path(), &["image", "mount"]);
    assert!(!mount.status.success());
    assert!(
        String::from_utf8_lossy(&mount.stderr).contains("must be specified"),
        "{}",
        String::from_utf8_lossy(&mount.stderr)
    );
}

#[test]
fn image_unmount_is_a_real_no_op_that_prints_the_short_id() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/image-unmount:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let normalized = Reference::parse("ociman-test/image-unmount:latest")
        .unwrap()
        .to_string();
    let record = store.resolve_image(&normalized).unwrap().unwrap();

    let unmount = ociman(
        storage_dir.path(),
        &["image", "unmount", "ociman-test/image-unmount:latest"],
    );
    assert!(
        unmount.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&unmount.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&unmount.stdout).trim(),
        &record.manifest_digest.hex()[..12]
    );

    // The image itself, and its manifest/blobs, must survive
    // completely untouched -- a real no-op, not a real teardown.
    let images = ociman(storage_dir.path(), &["images", "-q"]);
    assert!(!String::from_utf8_lossy(&images.stdout).trim().is_empty());
}

#[test]
fn image_unmount_umount_alias_works_too() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/image-umount-alias:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let umount = ociman(
        storage_dir.path(),
        &["image", "umount", "ociman-test/image-umount-alias:latest"],
    );
    assert!(
        umount.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&umount.stderr)
    );
}

#[test]
fn image_unmount_of_an_unknown_image_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let unmount = ociman(storage_dir.path(), &["image", "unmount", "never-pulled"]);
    assert!(!unmount.status.success());
    assert!(
        String::from_utf8_lossy(&unmount.stderr).contains("no such image"),
        "{}",
        String::from_utf8_lossy(&unmount.stderr)
    );
}
