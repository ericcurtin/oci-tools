//! `ociman load` integration tests: reading a real `oci-archive`
//! archive back into local storage, matching real `podman load`/
//! `docker load` (see `docs/design/0166`). Same fully offline seeded-
//! image approach `ociman_save.rs`/`ociman_tag.rs` established --
//! real, live `podman load`/`podman save` interop (loading a real
//! `podman`-produced archive, and a real `podman run` of an image
//! this binary loaded) was additionally verified by hand during this
//! feature's own development, since a real `podman`/`docker` binary
//! is not a dependency this automated suite can assume is present
//! everywhere it runs -- see `docs/design/0166` for that record.

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
fn load_of_a_non_archive_file_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let input = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(input.path(), b"not a tar file at all").unwrap();

    let load = ociman(
        storage_dir.path(),
        &["load", "-i", input.path().to_str().unwrap()],
    );
    assert!(!load.status.success());
}

#[test]
fn load_of_a_missing_input_file_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let load = ociman(
        storage_dir.path(),
        &["load", "-i", "/nonexistent/path/to/nothing.tar"],
    );
    assert!(!load.status.success());
    assert!(
        String::from_utf8_lossy(&load.stderr).contains("opening"),
        "{}",
        String::from_utf8_lossy(&load.stderr)
    );
}

/// The most convincing check, exercised entirely through the real
/// CLI: seed an image into one isolated store, `ociman save` it,
/// `ociman load` the resulting archive into a completely separate
/// store, and confirm the loaded image is fully usable there --
/// listed, inspectable, and (via a real `ociman run`) actually
/// runnable.
#[test]
fn save_then_load_round_trips_through_the_real_cli_into_a_usable_image() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let source_dir = tempfile::tempdir().unwrap();
    let source_store = Store::open(source_dir.path()).unwrap();
    seed_image(
        &source_store,
        "ociman-test/load-round-trip:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let source_record = source_store
        .resolve_image("docker.io/ociman-test/load-round-trip:latest")
        .unwrap()
        .unwrap();

    let archive_path = source_dir.path().join("out.tar");
    let save = ociman(
        source_dir.path(),
        &[
            "save",
            "-o",
            archive_path.to_str().unwrap(),
            "ociman-test/load-round-trip:latest",
        ],
    );
    assert!(
        save.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&save.stderr)
    );

    let dest_dir = tempfile::tempdir().unwrap();
    let load = ociman(
        dest_dir.path(),
        &["load", "-i", archive_path.to_str().unwrap()],
    );
    assert!(
        load.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&load.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&load.stdout).trim(),
        "Loaded image: docker.io/ociman-test/load-round-trip:latest"
    );

    let dest_store = Store::open(dest_dir.path()).unwrap();
    let dest_record = dest_store
        .resolve_image("docker.io/ociman-test/load-round-trip:latest")
        .unwrap()
        .unwrap();
    assert_eq!(dest_record.manifest_digest, source_record.manifest_digest);

    let images = ociman(dest_dir.path(), &["images"]);
    assert!(
        String::from_utf8_lossy(&images.stdout)
            .contains("docker.io/ociman-test/load-round-trip:latest")
    );

    let run = ociman(
        dest_dir.path(),
        &[
            "run",
            "--rm",
            "docker.io/ociman-test/load-round-trip:latest",
            "sh",
            "-c",
            "echo hello from a loaded image",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("hello from a loaded image"),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

#[test]
fn load_with_json_prints_the_reference_and_digest() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let source_dir = tempfile::tempdir().unwrap();
    let source_store = Store::open(source_dir.path()).unwrap();
    seed_image(
        &source_store,
        "ociman-test/load-json:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let source_record = source_store
        .resolve_image("docker.io/ociman-test/load-json:latest")
        .unwrap()
        .unwrap();

    let archive_path = source_dir.path().join("out.tar");
    let save = ociman(
        source_dir.path(),
        &[
            "save",
            "-o",
            archive_path.to_str().unwrap(),
            "ociman-test/load-json:latest",
        ],
    );
    assert!(save.status.success());

    let dest_dir = tempfile::tempdir().unwrap();
    let load = ociman(
        dest_dir.path(),
        &["load", "-i", archive_path.to_str().unwrap(), "--json"],
    );
    assert!(
        load.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&load.stderr)
    );
    let parsed: serde_json::Value = serde_json::from_slice(&load.stdout).unwrap();
    assert_eq!(
        parsed["reference"],
        "docker.io/ociman-test/load-json:latest"
    );
    assert_eq!(parsed["digest"], source_record.manifest_digest.to_string());
}
