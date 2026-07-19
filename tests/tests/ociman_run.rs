//! `ociman run` integration tests, exercised entirely offline: no real
//! registry pull. Instead, a local `oci_store::Store` is hand-seeded
//! with a synthetic-but-structurally-real image — a real `busybox`
//! binary tarred and gzipped as the one layer, and a real
//! `ImageConfig`/`ImageManifest` (built from `oci_spec_types`, the same
//! types a real pull deserializes into) ingested exactly the way
//! `oci_registry::pull` would have left them. Deterministic, no network
//! dependency, and exercises the *same* extraction/spec-synthesis/
//! launch code path a real pulled image goes through — `ociman`'s own
//! `resolve_or_pull` finds the image already present and skips pulling
//! entirely, so nothing here is a special "test mode".
//!
//! This exact real-image-first approach is what caught a real,
//! previously-undetected bug while building this increment: real image
//! config JSON uses `PascalCase` field names (`Cmd`, `Env`, ...), which
//! `oci_spec_types::image::ContainerConfig` didn't declare — every
//! field silently deserialized to its default for every real image
//! ever pulled, until `ociman run` against an actual `busybox` image
//! produced an empty command. Fixed in `oci-spec-types` alongside this
//! increment; a real fixture-based test lives in `oci-spec-types`
//! itself, and the tests below would have caught the regression too
//! (a synthetic seeded image using the same real struct).

use std::io::Write as _;
use std::path::Path;
use std::process::Command;

use oci_spec_types::Reference;
use oci_spec_types::digest::sha256;
use oci_spec_types::image::{
    ContainerConfig, Descriptor, ImageManifest, MEDIA_TYPE_IMAGE_CONFIG,
    MEDIA_TYPE_IMAGE_LAYER_GZIP, MEDIA_TYPE_IMAGE_MANIFEST,
};
use oci_store::{ImageRecord, Store};

use oci_tools_tests::{bin_path, busybox_path, seed_image};

fn ociman_run(storage_root: &Path, image: &str, args: &[&str]) -> std::process::Output {
    Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
        .env_remove("OCI_TOOLS_LOG")
        .args(["run", image])
        .args(args)
        .output()
        .expect("failed to spawn ociman run")
}

#[test]
fn run_uses_the_images_default_cmd_and_env() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/default-cmd:latest",
        &busybox,
        &["sh", "echo", "env"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "echo hello-from-default-cmd; env | grep ^PATH=".to_string(),
            ]),
            env: vec!["PATH=/bin".to_string()],
            ..Default::default()
        },
    );

    let out = ociman_run(storage_dir.path(), "ociman-test/default-cmd:latest", &[]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("hello-from-default-cmd"),
        "got stdout: {stdout:?}"
    );
    assert!(stdout.contains("PATH=/bin"), "got stdout: {stdout:?}");
}

#[test]
fn run_args_override_the_images_default_cmd() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/override-cmd:latest",
        &busybox,
        &["sh", "echo"],
        ContainerConfig {
            cmd: Some(vec![
                "/bin/echo".to_string(),
                "default-cmd-unused".to_string(),
            ]),
            ..Default::default()
        },
    );

    let out = ociman_run(
        storage_dir.path(),
        "ociman-test/override-cmd:latest",
        &["/bin/echo", "overridden-args-used"],
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("overridden-args-used"));
    assert!(!stdout.contains("default-cmd-unused"));
}

#[test]
fn run_propagates_the_containers_exit_code() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/exit-code:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let out = ociman_run(
        storage_dir.path(),
        "ociman-test/exit-code:latest",
        &["/bin/sh", "-c", "exit 42"],
    );
    assert_eq!(
        out.status.code(),
        Some(42),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn run_rejects_a_non_root_numeric_user() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ociman-test/non-root-user:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            user: Some("1000".to_string()),
            ..Default::default()
        },
    );

    let out = ociman_run(
        storage_dir.path(),
        "ociman-test/non-root-user:latest",
        &["/bin/sh", "-c", "true"],
    );
    assert!(
        !out.status.success(),
        "run should refuse an image requesting a non-root numeric user \
         (see resolve_user's own doc comment: not mappable yet)"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("cannot map"), "got stderr: {stderr:?}");
}

/// The bug this whole increment's real-image-first testing approach was
/// built to catch (see this file's own doc comment): a `ContainerConfig`
/// without the right wire casing silently loses `Cmd`/`Env`. Guards
/// against ever regressing that in a way visible from `ociman run`
/// itself, not just `oci-spec-types`'s own unit test.
#[test]
fn run_actually_uses_cmd_from_a_pascal_case_wire_config() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();

    // Seed via raw PascalCase JSON bytes, not the `ContainerConfig`
    // struct — proves the wire format round-trips through real
    // deserialization exactly like a real registry blob would.
    let mut builder = tar::Builder::new(Vec::new());
    let busybox_bytes = std::fs::read(&busybox).unwrap();
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(busybox_bytes.len() as u64);
    header.set_mode(0o755);
    builder
        .append_data(&mut header, "bin/busybox", busybox_bytes.as_slice())
        .unwrap();
    let mut link_header = tar::Header::new_gnu();
    link_header.set_entry_type(tar::EntryType::Symlink);
    link_header.set_mode(0o777);
    link_header.set_size(0);
    builder
        .append_link(&mut link_header, "bin/sh", "busybox")
        .unwrap();
    let tar_bytes = builder.into_inner().unwrap();
    let diff_id = sha256(&tar_bytes);
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&tar_bytes).unwrap();
    let gzipped = encoder.finish().unwrap();
    let layer = store.ingest(gzipped.as_slice()).unwrap();

    let raw_config = serde_json::json!({
        "architecture": std::env::consts::ARCH,
        "os": "linux",
        "config": {
            "Cmd": ["/bin/sh", "-c", "echo pascal-case-cmd-worked"],
        },
        "rootfs": {"type": "layers", "diff_ids": [diff_id.to_string()]},
    });
    let config = store
        .ingest(serde_json::to_vec(&raw_config).unwrap().as_slice())
        .unwrap();

    let manifest = ImageManifest {
        schema_version: 2,
        media_type: Some(MEDIA_TYPE_IMAGE_MANIFEST.to_string()),
        config: Descriptor {
            media_type: MEDIA_TYPE_IMAGE_CONFIG.to_string(),
            digest: config.digest,
            size: config.size,
            urls: vec![],
            annotations: Default::default(),
            platform: None,
        },
        layers: vec![Descriptor {
            media_type: MEDIA_TYPE_IMAGE_LAYER_GZIP.to_string(),
            digest: layer.digest,
            size: layer.size,
            urls: vec![],
            annotations: Default::default(),
            platform: None,
        }],
        annotations: Default::default(),
    };
    let manifest_ingested = store
        .ingest(serde_json::to_vec(&manifest).unwrap().as_slice())
        .unwrap();
    let normalized = Reference::parse("ociman-test/pascal-case:latest")
        .unwrap()
        .to_string();
    store
        .put_image(&ImageRecord {
            reference: normalized,
            manifest_digest: manifest_ingested.digest,
        })
        .unwrap();

    let out = ociman_run(storage_dir.path(), "ociman-test/pascal-case:latest", &[]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("pascal-case-cmd-worked"),
        "got stdout: {stdout:?}"
    );
}
