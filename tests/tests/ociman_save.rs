//! `ociman save` integration tests: writing an already-stored image
//! out as a real, self-contained `oci-archive`-format tar file,
//! matching real `podman save --format oci-archive`/`docker save
//! --format oci-archive` (see `docs/design/0165`). Same fully offline
//! seeded-image approach `ociman_run.rs`/`ociman_tag.rs` established
//! -- the archive's own structural correctness (`oci-layout`,
//! `index.json`, every blob byte-for-byte) is checked here directly by
//! re-reading the produced tar; real, live `podman load` interop was
//! additionally verified by hand during this feature's own
//! development (a real `podman load` of an archive this binary
//! produced round-tripped: correct tag, correct arch/os, and the
//! loaded image actually ran) -- see `docs/design/0165` for that
//! record, since a real `podman`/`docker` binary is not a dependency
//! this automated suite can assume is present everywhere it runs.

use std::collections::BTreeMap;
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

fn read_tar_entries(bytes: &[u8]) -> BTreeMap<String, Vec<u8>> {
    let mut archive = tar::Archive::new(bytes);
    let mut entries = BTreeMap::new();
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        if entry.header().entry_type() == tar::EntryType::Directory {
            continue;
        }
        let path = entry.path().unwrap().to_str().unwrap().to_string();
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut buf).unwrap();
        entries.insert(path, buf);
    }
    entries
}

#[test]
fn save_of_an_unknown_reference_is_a_clear_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let output = tempfile::NamedTempFile::new().unwrap();
    let save = ociman(
        storage_dir.path(),
        &[
            "save",
            "-o",
            output.path().to_str().unwrap(),
            "ociman-test/never-pulled-or-built:latest",
        ],
    );
    assert!(!save.status.success());
    assert!(
        String::from_utf8_lossy(&save.stderr).contains("no such image"),
        "{}",
        String::from_utf8_lossy(&save.stderr)
    );
}

#[test]
fn save_rejects_an_unimplemented_format_before_touching_the_store() {
    let storage_dir = tempfile::tempdir().unwrap();
    let output = tempfile::NamedTempFile::new().unwrap();
    let save = ociman(
        storage_dir.path(),
        &[
            "save",
            "--format",
            "docker-archive",
            "-o",
            output.path().to_str().unwrap(),
            "anything:latest",
        ],
    );
    assert!(!save.status.success());
    let stderr = String::from_utf8_lossy(&save.stderr);
    assert!(
        stderr.contains("docker-archive") && stderr.contains("oci-archive"),
        "{stderr}"
    );
}

#[test]
fn save_writes_a_real_oci_archive_with_every_expected_file_and_exact_blob_bytes() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/save-source:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let record = store
        .resolve_image("docker.io/ociman-test/save-source:latest")
        .unwrap()
        .unwrap();
    let manifest = store.image_manifest(&record).unwrap();
    let manifest_bytes = store.read_blob(&record.manifest_digest).unwrap();
    let config_bytes = store.read_blob(&manifest.config.digest).unwrap();

    let output_path = storage_dir.path().join("out.tar");
    let save = ociman(
        storage_dir.path(),
        &[
            "save",
            "--format",
            "oci-archive",
            "-o",
            output_path.to_str().unwrap(),
            "ociman-test/save-source:latest",
        ],
    );
    assert!(
        save.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&save.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&save.stdout).trim(),
        record.manifest_digest.to_string()
    );

    let archive_bytes = std::fs::read(&output_path).unwrap();
    let entries = read_tar_entries(&archive_bytes);

    assert_eq!(
        entries.get("oci-layout").unwrap(),
        br#"{"imageLayoutVersion":"1.0.0"}"#
    );

    let index: oci_spec_types::image::ImageIndex =
        serde_json::from_slice(entries.get("index.json").unwrap()).unwrap();
    assert_eq!(index.schema_version, 2);
    assert!(index.media_type.is_none());
    assert_eq!(index.manifests.len(), 1);
    assert_eq!(index.manifests[0].digest, record.manifest_digest);
    assert_eq!(
        index.manifests[0]
            .annotations
            .get("org.opencontainers.image.ref.name")
            .unwrap(),
        "docker.io/ociman-test/save-source:latest"
    );

    assert_eq!(
        entries
            .get(&format!("blobs/sha256/{}", record.manifest_digest.hex()))
            .unwrap(),
        &manifest_bytes
    );
    assert_eq!(
        entries
            .get(&format!("blobs/sha256/{}", manifest.config.digest.hex()))
            .unwrap(),
        &config_bytes
    );
    for layer in &manifest.layers {
        let layer_bytes = store.read_blob(&layer.digest).unwrap();
        assert_eq!(
            entries
                .get(&format!("blobs/sha256/{}", layer.digest.hex()))
                .unwrap(),
            &layer_bytes
        );
    }
}

#[test]
fn save_with_no_output_flag_writes_the_archive_straight_to_stdout_and_nothing_else() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/save-stdout:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let record = store
        .resolve_image("docker.io/ociman-test/save-stdout:latest")
        .unwrap()
        .unwrap();

    let save = ociman(
        storage_dir.path(),
        &["save", "ociman-test/save-stdout:latest"],
    );
    assert!(
        save.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&save.stderr)
    );

    // Stdout is *only* ever the archive bytes -- no digest line, no
    // JSON, nothing -- see `SaveResult`'s own doc comment in
    // `bin/ociman/src/main.rs` for why.
    let entries = read_tar_entries(&save.stdout);
    assert!(entries.contains_key("oci-layout"));
    assert!(entries.contains_key("index.json"));
    assert!(entries.contains_key(&format!("blobs/sha256/{}", record.manifest_digest.hex())));
}

#[test]
fn save_resolves_by_a_short_image_id_the_same_way_push_does() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/save-by-id:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let record = store
        .resolve_image("docker.io/ociman-test/save-by-id:latest")
        .unwrap()
        .unwrap();
    let short_id = &record.manifest_digest.hex()[..12];

    let output_path = storage_dir.path().join("out.tar");
    let save = ociman(
        storage_dir.path(),
        &["save", "-o", output_path.to_str().unwrap(), short_id],
    );
    assert!(
        save.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&save.stderr)
    );
    let entries = read_tar_entries(&std::fs::read(&output_path).unwrap());
    assert!(entries.contains_key(&format!("blobs/sha256/{}", record.manifest_digest.hex())));
}
