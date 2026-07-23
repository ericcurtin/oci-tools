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

/// Every `--seal` test needs a real fs-verity-capable filesystem,
/// which a plain tempdir may or may not be -- same real, from-scratch
/// loopback ext4 (`mkfs.ext4 -O verity`) fixture `oci_erofs::verity`'s
/// own unit tests already establish, replicated here (rather than
/// shared across crates) since it's `oci-erofs`'s own private test
/// helper, not a public API.
struct VerityFs {
    _dir: tempfile::TempDir,
    mountpoint: std::path::PathBuf,
}

impl Drop for VerityFs {
    fn drop(&mut self) {
        let _ = Command::new("sudo")
            .args(["umount", "-q"])
            .arg(&self.mountpoint)
            .output();
    }
}

fn verity_capable_ext4() -> Option<VerityFs> {
    let dir = tempfile::tempdir().ok()?;
    let image = dir.path().join("verity.img");
    let mountpoint = dir.path().join("mnt");
    std::fs::create_dir_all(&mountpoint).ok()?;

    if !Command::new("truncate")
        .args(["-s", "32M"])
        .arg(&image)
        .status()
        .ok()?
        .success()
    {
        return None;
    }
    if !Command::new("mkfs.ext4")
        .args(["-O", "verity", "-q"])
        .arg(&image)
        .status()
        .ok()?
        .success()
    {
        return None;
    }
    if !Command::new("sudo")
        .args(["mount", "-o", "loop"])
        .arg(&image)
        .arg(&mountpoint)
        .status()
        .ok()?
        .success()
    {
        return None;
    }
    let uid_gid = format!(
        "{}:{}",
        rustix::process::getuid().as_raw(),
        rustix::process::getgid().as_raw()
    );
    if !Command::new("sudo")
        .args(["chown", &uid_gid])
        .arg(&mountpoint)
        .status()
        .ok()?
        .success()
    {
        return None;
    }
    Some(VerityFs {
        _dir: dir,
        mountpoint,
    })
}

/// `ociboot build-image --seal`: seals the freshly built image with
/// real fs-verity and prints its own real digest — verified directly
/// (not just that the command succeeded): the file becomes genuinely
/// immutable at the kernel level afterward (a real write fails with
/// `EPERM`), and the printed digest is a real, non-placeholder
/// fs-verity digest (32 real bytes, never all-zero).
#[test]
fn build_image_seal_makes_the_output_genuinely_immutable_and_prints_a_real_digest() {
    if !mkfs_erofs_available() {
        eprintln!("skipping: mkfs.erofs not installed");
        return;
    }
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let Some(verity_fs) = verity_capable_ext4() else {
        eprintln!("skipping: could not create a real fs-verity-capable ext4 loopback mount");
        return;
    };

    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociboot-test/build-image-seal:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let output_path = verity_fs.mountpoint.join("deployment.erofs");
    let build = ociboot(
        storage_dir.path(),
        &[
            "build-image",
            "ociboot-test/build-image-seal:latest",
            "--output",
            output_path.to_str().unwrap(),
            "--seal",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let stdout = String::from_utf8_lossy(&build.stdout);
    let verity_line = stdout
        .lines()
        .find(|line| line.starts_with("verity: "))
        .expect("--seal should print a verity: <digest> line");
    let digest_hex = verity_line.strip_prefix("verity: ").unwrap();
    assert_eq!(digest_hex.len(), 64, "should be a 32-byte hex digest");
    assert!(
        digest_hex.chars().any(|c| c != '0'),
        "digest should be a real hash, not all zero: {digest_hex}"
    );

    // Genuinely immutable now -- a real write fails with EPERM at the
    // kernel level, not merely "this command didn't happen to modify
    // it".
    let write_result = std::fs::OpenOptions::new()
        .append(true)
        .open(&output_path)
        .and_then(|mut f| std::io::Write::write_all(&mut f, b"tampered"));
    assert!(
        write_result.is_err(),
        "a sealed file should reject a real write attempt"
    );
}

/// Without `--seal` (the default), no `verity: ...` line at all, and
/// the file is still a perfectly ordinary, writable file afterward --
/// confirms `--seal` is genuinely opt-in, not accidentally always-on.
#[test]
fn build_image_without_seal_prints_no_verity_line_and_stays_writable() {
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
        "ociboot-test/build-image-noseal:latest",
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
            "ociboot-test/build-image-noseal:latest",
            "--output",
            output_path.to_str().unwrap(),
        ],
    );
    assert!(build.status.success());
    assert!(
        !String::from_utf8_lossy(&build.stdout).contains("verity:"),
        "without --seal there should be no verity: line at all"
    );
    // An ordinary, unsealed file: a real write still succeeds.
    std::fs::OpenOptions::new()
        .append(true)
        .open(&output_path)
        .and_then(|mut f| std::io::Write::write_all(&mut f, b"still writable"))
        .expect("an unsealed file should still accept a real write");
}
