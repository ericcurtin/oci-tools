//! `ociman load` integration tests: reading a real archive (either
//! format, auto-detected) back into local storage, matching real
//! `podman load`/`docker load` (see `docs/design/0166`/`0168`). Same
//! fully offline seeded-image approach `ociman_save.rs`/
//! `ociman_tag.rs` established -- real, live `podman load`/`docker
//! load`/`podman save`/`docker save` interop (loading a real
//! `podman`/`docker`-produced archive of each format, and a real
//! `podman run`/`docker run` of an image this binary loaded, plus
//! loading this binary's own `docker-archive` output with both real
//! tools) was additionally verified by hand during each feature's own
//! development, since a real `podman`/`docker` binary is not a
//! dependency this automated suite can assume is present everywhere
//! it runs -- see `docs/design/0166`/`0167`/`0168` for those records.
//! `docker-archive`-specific read-side coverage (manifest
//! synthesis, `diff_id` cross-checking, multi-`RepoTags` tagging)
//! lives in `bin/ociman/src/archive.rs`'s own unit tests instead,
//! since building a docker-archive tar by hand is easier to do
//! directly against the store than through this file's own CLI-only
//! `ociman` helper.

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
        parsed["references"],
        serde_json::json!(["docker.io/ociman-test/load-json:latest"])
    );
    assert_eq!(parsed["digest"], source_record.manifest_digest.to_string());
}

/// An untagged image (0179) round-trips through `save`/`load`
/// correctly: no bogus reference gets embedded in the archive (which
/// would otherwise `Reference::parse` this project's own internal
/// sentinel string into a nonsense tag on the far end), `load` reports
/// zero references (matching real `podman load`'s own "no tag"
/// output), and the destination store still records it, findable by
/// ID and shown as `<none>` by `ociman images`, exactly like the
/// source did -- not silently dropped/orphaned.
#[test]
fn save_then_load_round_trips_an_untagged_image_still_untagged() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let source_dir = tempfile::tempdir().unwrap();
    let source_store = Store::open(source_dir.path()).unwrap();
    seed_image(
        &source_store,
        "ociman-test/untagged-load-base:latest",
        &busybox,
        &["sh", "cat"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        context_dir.path().join("Containerfile"),
        "FROM ociman-test/untagged-load-base:latest\nRUN echo hi > /hi.txt\n",
    )
    .unwrap();
    let build = ociman(
        source_dir.path(),
        &["build", context_dir.path().to_str().unwrap()],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let digest = String::from_utf8_lossy(&build.stdout)
        .lines()
        .next()
        .unwrap()
        .to_string();
    let short_id = &digest.trim_start_matches("sha256:")[..12];

    let archive_path = source_dir.path().join("untagged.tar");
    let save = ociman(
        source_dir.path(),
        &["save", "-o", archive_path.to_str().unwrap(), short_id],
    );
    assert!(
        save.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&save.stderr)
    );

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
        parsed["references"],
        serde_json::json!([]),
        "an untagged image should still report zero real references after loading"
    );
    assert_eq!(parsed["digest"], digest);

    let images = ociman(dest_dir.path(), &["images", "--json"]);
    assert!(images.status.success());
    let views: serde_json::Value = serde_json::from_slice(&images.stdout).unwrap();
    let loaded = views
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["digest"] == digest)
        .expect("the untagged image should still show up in the destination store's listing");
    assert_eq!(loaded["reference"], serde_json::Value::Null);

    // `ociman run` has no image-by-ID resolution of its own (a
    // separate, pre-existing gap, unrelated to 0179 -- `ociman tag`/
    // `inspect`/`rmi`/`push`/`save` all already do via 0122's own
    // `resolve_image_by_reference_or_id`, `run` never has), so
    // runnability is checked here via a real tag applied to the
    // already-loaded, still-untagged image first.
    let short_id = &digest.trim_start_matches("sha256:")[..12];
    let tag = ociman(
        dest_dir.path(),
        &["tag", short_id, "ociman-test/untagged-load-result:latest"],
    );
    assert!(
        tag.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&tag.stderr)
    );
    let run = ociman(
        dest_dir.path(),
        &[
            "run",
            "--rm",
            "ociman-test/untagged-load-result:latest",
            "cat",
            "/hi.txt",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "hi\n");
}
