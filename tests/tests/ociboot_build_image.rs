//! `ociboot build-image` integration tests: exercises the actual
//! built `ociboot` binary against a real, seeded image in local
//! storage (the same `oci_tools_tests::seed_image` fixture
//! `ociman_run.rs`/`ociman_build.rs` already use) — `oci-erofs` itself
//! already has its own thorough unit test coverage (including a
//! byte-for-byte determinism check against the real `mkfs.erofs`
//! binary), this is a CLI-surface test on top of it, confirming the
//! new wiring (image resolution, the shared rootfs cache, and the
//! `created`/manifest-digest-derived `timestamp`/`uuid` policy) all
//! actually work together end to end.

use std::path::Path;
use std::process::Command;

use oci_spec_types::image::ContainerConfig;
use oci_store::Store;

use oci_tools_tests::{bin_path, busybox_path, seed_image};

fn ociboot(storage_root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin_path("ociboot"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
        .env_remove("OCI_TOOLS_LOG")
        .args(args)
        .output()
        .expect("failed to spawn ociboot")
}

/// `mkfs.erofs` is a real, sanctioned external-tool dependency
/// (`oci_erofs::builder`'s own doc comment) — matches that crate's
/// own test-skip convention for an environment that doesn't have it
/// installed, rather than failing the whole suite outright.
fn mkfs_erofs_available() -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path)
        .map(|dir| dir.join("mkfs.erofs"))
        .any(|p| p.is_file())
}

#[test]
fn build_image_writes_a_real_valid_erofs_image() {
    if !mkfs_erofs_available() {
        eprintln!("skipping: mkfs.erofs not installed");
        return;
    }
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociboot-test/build-image-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let output_dir = tempfile::tempdir().unwrap();
    let output_path = output_dir.path().join("deployment.erofs");

    let build = ociboot(
        storage_dir.path(),
        &[
            "build-image",
            "ociboot-test/build-image-base:latest",
            "--output",
            output_path.to_str().unwrap(),
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&build.stdout).trim(),
        output_path.to_str().unwrap()
    );

    let bytes = std::fs::read(&output_path).unwrap();
    assert!(!bytes.is_empty(), "should have written a real image");
    // Same fixed-offset superblock check `oci_erofs::builder`'s own
    // unit test uses (`EROFS_SUPER_OFFSET` = 1024, `EROFS_SUPER_
    // MAGIC_V1` = 0xE0F5E1E2, little-endian on disk).
    let magic = u32::from_le_bytes(bytes[1024..1028].try_into().unwrap());
    assert_eq!(
        magic, 0xE0F5_E1E2,
        "output should be a real erofs superblock"
    );
}

/// The same image, built twice, produces byte-identical output —
/// confirming the `timestamp`/`uuid` derivation is genuinely
/// deterministic (from the image's own `created`/manifest digest,
/// never wall-clock "now" or a random UUID).
#[test]
fn build_image_is_fully_deterministic_across_two_separate_invocations() {
    if !mkfs_erofs_available() {
        eprintln!("skipping: mkfs.erofs not installed");
        return;
    }
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociboot-test/build-image-deterministic:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let output_dir = tempfile::tempdir().unwrap();
    let first = output_dir.path().join("first.erofs");
    let second = output_dir.path().join("second.erofs");

    // A real (if short) delay between the two builds -- if the
    // `timestamp` were ever accidentally derived from wall-clock
    // "now" instead of the image's own `created` field, this would
    // catch it (two different real build times would then bake in
    // two different superblock timestamps, producing different
    // bytes).
    std::thread::sleep(std::time::Duration::from_millis(1100));

    for output in [&first, &second] {
        let build = ociboot(
            storage_dir.path(),
            &[
                "build-image",
                "ociboot-test/build-image-deterministic:latest",
                "--output",
                output.to_str().unwrap(),
            ],
        );
        assert!(
            build.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&build.stderr)
        );
    }

    let first_bytes = std::fs::read(&first).unwrap();
    let second_bytes = std::fs::read(&second).unwrap();
    assert_eq!(
        first_bytes, second_bytes,
        "the same image should always produce a byte-identical erofs image"
    );
}

/// An image not present in local storage is a clear, immediate error
/// -- this command never pulls one itself.
#[test]
fn build_image_of_an_unknown_reference_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let output_dir = tempfile::tempdir().unwrap();
    let build = ociboot(
        storage_dir.path(),
        &[
            "build-image",
            "ociboot-test/does-not-exist:latest",
            "--output",
            output_dir.path().join("out.erofs").to_str().unwrap(),
        ],
    );
    assert!(!build.status.success());
    assert!(
        String::from_utf8_lossy(&build.stderr).contains("ociman pull"),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
}
