//! Cross-binary integration tests for the oci-tools workspace.
//!
//! The actual tests live in `tests/tests/*.rs`. They exercise the built
//! binaries (`target/<profile>/<bin>`), so run them via a full workspace
//! invocation which builds all bin targets first:
//!
//! ```sh
//! cargo build --workspace && cargo test --workspace
//! ```
//!
//! Later milestones add lifecycle suites here: ociman build/run/exec
//! (rootless + root), ocirun runtime-spec conformance, the ocicri critest
//! subset, and the ociboot QEMU full-boot test.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use oci_spec_types::Reference;
use oci_spec_types::digest::sha256;
use oci_spec_types::image::{
    ContainerConfig, Descriptor, ImageConfig, ImageManifest, MEDIA_TYPE_IMAGE_CONFIG,
    MEDIA_TYPE_IMAGE_LAYER_GZIP, MEDIA_TYPE_IMAGE_LAYER_ZSTD, MEDIA_TYPE_IMAGE_MANIFEST, RootFs,
};
use oci_store::{ImageRecord, Store};

/// Locate a workspace binary next to this test executable's target dir.
/// Shared by every file under `tests/tests/*.rs` so there is exactly one
/// implementation of "where did `cargo build --workspace` put the
/// binaries".
pub fn bin_path(name: &str) -> PathBuf {
    let mut path = std::env::current_exe().expect("current_exe");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.push(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    assert!(
        path.exists(),
        "binary {name} not found at {}; run `cargo build --workspace` first",
        path.display()
    );
    path
}

/// Locate `busybox`, or `None` if it isn't installed. Every real
/// `ocirun` end-to-end test needs a minimal rootfs to `exec` something
/// in; `busybox` is present in this project's dev environment and
/// common on minimal cloud images, but isn't installed by `ci/
/// vm-prepare.sh`, so tests using it skip themselves — printing why, not
/// failing — when it isn't found, rather than making it a hard CI
/// dependency.
pub fn busybox_path() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("busybox"))
        .find(|p| p.is_file())
}

/// Build a minimal bundle at `dir`: a busybox-based rootfs with `sh` and
/// the given symlinked applets, and a rootless `config.json` running
/// `args` (a `/bin/sh -c "..."` style command is the expected shape).
pub fn write_bundle(dir: &Path, busybox: &Path, args: &[&str]) {
    let rootfs = dir.join("rootfs");
    std::fs::create_dir_all(rootfs.join("bin")).unwrap();
    std::fs::copy(busybox, rootfs.join("bin/busybox")).unwrap();
    for applet in ["sh", "echo", "true", "false"] {
        #[cfg(unix)]
        std::os::unix::fs::symlink("busybox", rootfs.join("bin").join(applet)).unwrap();
    }

    let out = Command::new(bin_path("ocirun"))
        .args(["spec", "--rootless", "--bundle"])
        .arg(dir)
        .output()
        .expect("failed to spawn ocirun spec");
    assert!(
        out.status.success(),
        "ocirun spec --rootless failed: {out:?}"
    );

    let config_path = dir.join("config.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    config["process"]["terminal"] = serde_json::json!(false);
    config["process"]["args"] = serde_json::json!(args);
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
}

/// Seed `store` with a synthetic single-layer image at `reference`,
/// whose one layer is a real `busybox` binary at `bin/busybox` plus the
/// given symlinked applets, and whose `ContainerConfig` is
/// `container_config`. Afterward the image is retrievable exactly like
/// a real `ociman pull` would have left it — used by every `ociman
/// run`/`ps`/`rm` test that needs a real image without a real registry
/// (see `tests/tests/ociman_run.rs`'s own doc comment for why this
/// approach exists: it's what caught a real `ContainerConfig` wire-
/// casing bug that a hand-written fixture alone did not).
pub fn seed_image(
    store: &Store,
    reference: &str,
    busybox: &Path,
    applets: &[&str],
    container_config: ContainerConfig,
) {
    seed_image_with_files(store, reference, busybox, applets, &[], container_config);
}

/// [`seed_image`], plus `extra_files` — additional regular files (path
/// relative to the rootfs root, contents) baked into the same layer.
/// Used by tests that need something beyond a bare busybox rootfs, e.g.
/// an `/etc/passwd` to resolve a named image `USER` against.
pub fn seed_image_with_files(
    store: &Store,
    reference: &str,
    busybox: &Path,
    applets: &[&str],
    extra_files: &[(&str, &[u8])],
    container_config: ContainerConfig,
) {
    seed_image_with_files_and_compression(
        store,
        reference,
        busybox,
        applets,
        extra_files,
        LayerCompression::Gzip,
        container_config,
    );
}

/// Which compression [`seed_image_with_files_and_compression`] applies
/// to its one synthetic layer, and correspondingly which media type it
/// declares in the manifest — real registries serve either, and
/// `ociman`'s own `compression_for_media_type` has to handle both (see
/// `tests/tests/ociman_run.rs`'s zstd test).
#[derive(Debug, Clone, Copy)]
pub enum LayerCompression {
    /// `tar+gzip` (the overwhelmingly common real-world case).
    Gzip,
    /// `tar+zstd`.
    Zstd,
}

/// [`seed_image_with_files`], plus an explicit `compression` choice
/// instead of always gzip.
pub fn seed_image_with_files_and_compression(
    store: &Store,
    reference: &str,
    busybox: &Path,
    applets: &[&str],
    extra_files: &[(&str, &[u8])],
    compression: LayerCompression,
    container_config: ContainerConfig,
) {
    let mut builder = tar::Builder::new(Vec::new());
    let busybox_bytes = std::fs::read(busybox).unwrap();
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(busybox_bytes.len() as u64);
    header.set_mode(0o755);
    builder
        .append_data(&mut header, "bin/busybox", busybox_bytes.as_slice())
        .unwrap();
    for applet in applets {
        let mut link_header = tar::Header::new_gnu();
        link_header.set_entry_type(tar::EntryType::Symlink);
        link_header.set_mode(0o777);
        link_header.set_size(0);
        builder
            .append_link(&mut link_header, format!("bin/{applet}"), "busybox")
            .unwrap();
    }
    for (path, contents) in extra_files {
        let mut file_header = tar::Header::new_gnu();
        file_header.set_entry_type(tar::EntryType::Regular);
        file_header.set_size(contents.len() as u64);
        file_header.set_mode(0o644);
        builder
            .append_data(&mut file_header, path, *contents)
            .unwrap();
    }
    let tar_bytes = builder.into_inner().unwrap();
    let diff_id = sha256(&tar_bytes);

    // Compressed exactly like a real registry blob would be, gzip or
    // zstd depending on what the caller asked for.
    let (compressed, media_type) = match compression {
        LayerCompression::Gzip => {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(&tar_bytes).unwrap();
            (encoder.finish().unwrap(), MEDIA_TYPE_IMAGE_LAYER_GZIP)
        }
        LayerCompression::Zstd => {
            let compressed = ruzstd::encoding::compress_to_vec(
                tar_bytes.as_slice(),
                ruzstd::encoding::CompressionLevel::Fastest,
            );
            (compressed, MEDIA_TYPE_IMAGE_LAYER_ZSTD)
        }
    };
    let layer = store.ingest(compressed.as_slice()).unwrap();

    let image_config = ImageConfig {
        architecture: Some(std::env::consts::ARCH.to_string()),
        os: Some("linux".to_string()),
        created: None,
        author: None,
        config: Some(container_config),
        rootfs: RootFs {
            kind: "layers".to_string(),
            diff_ids: vec![diff_id],
        },
        history: vec![],
    };
    let config = store
        .ingest(serde_json::to_vec(&image_config).unwrap().as_slice())
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
            media_type: media_type.to_string(),
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

    let normalized = Reference::parse(reference).unwrap().to_string();
    store
        .put_image(&ImageRecord {
            reference: normalized,
            manifest_digest: manifest_ingested.digest,
        })
        .unwrap();
}

/// Run `ocirun --root <root> <args...>`, capturing its output.
pub fn ocirun(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin_path("ocirun"))
        .arg("--root")
        .arg(root)
        .args(args)
        .env_remove("OCI_TOOLS_LOG")
        .output()
        .expect("failed to spawn ocirun")
}

/// `ocirun create <id> --bundle <bundle>`, with stdio explicitly
/// detached from this test process's own captured pipes: `create`
/// leaves the container's own process running in the background (see
/// `docs/design/0017`), and it inherits whatever `create` had — a real
/// pipe (like the one `Command::output()` otherwise sets up to capture
/// output) would never see EOF until *every* process holding a copy of
/// it exits, hanging this test process's own `output()` call for as
/// long as the container itself keeps running. Caught by hitting
/// exactly that hang once while manually verifying `create`/`start`
/// against a real kernel, not foreseen in advance.
pub fn ocirun_create(root: &Path, bundle: &Path, id: &str) -> std::process::Output {
    Command::new(bin_path("ocirun"))
        .arg("--root")
        .arg(root)
        .args(["create", id, "--bundle"])
        .arg(bundle)
        .env_remove("OCI_TOOLS_LOG")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .expect("failed to spawn ocirun create")
}

/// The `status` field from `ocirun state <id>` (as plain JSON output),
/// asserting the command itself succeeded.
pub fn state_status(root: &Path, id: &str) -> String {
    let out = ocirun(root, &["state", id]);
    assert!(
        out.status.success(),
        "ocirun state failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    json["status"].as_str().unwrap().to_string()
}

/// The `status` field for `id` out of `ocirun list --format json` (as
/// plain JSON output), asserting the command itself succeeded and
/// that `id` is actually present in the list.
pub fn list_status(root: &Path, id: &str) -> String {
    let out = ocirun(root, &["list", "--format", "json"]);
    assert!(
        out.status.success(),
        "ocirun list failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let views = json
        .as_array()
        .expect("ocirun list --format json is an array");
    let view = views
        .iter()
        .find(|v| v["id"].as_str() == Some(id))
        .unwrap_or_else(|| panic!("{id:?} not found in ocirun list output: {views:?}"));
    view["status"].as_str().unwrap().to_string()
}

/// Poll [`state_status`] until it equals `want` or `timeout` elapses
/// (status transitions — e.g. a killed container becoming "stopped" —
/// aren't necessarily instantaneous from a separate process's point of
/// view).
pub fn wait_for_status(root: &Path, id: &str, want: &str, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let status = state_status(root, id);
        if status == want || Instant::now() >= deadline {
            return status;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
