//! `ociman run --quiet`/`ociman create --quiet` (0439): exercises the
//! actual built `ociman` binary's own `--quiet`/`-q` flag against a
//! real, local, anonymous (no-auth) plain-HTTP mock registry serving
//! a real, *extractable* single-layer busybox image — unlike
//! `ociman_tls_verify.rs`'s/`ociman_pull_policy.rs`'s own mock
//! registries (whose one layer is deliberately fake, non-tar content,
//! fine for `pull`/`push`/`build`'s own metadata-only needs but not
//! for `create`/`run`, which both need a real rootfs to extract into)
//! -- same real gzip-tar-layer construction technique
//! `oci_tools_tests::seed_image_with_files_and_compression` already
//! establishes for the fully-local (no HTTP) case, reused here to
//! build real bytes for the mock registry's own blob route instead of
//! writing straight into a `Store`.
//!
//! Real `podman run --quiet`/`podman create --quiet` were both
//! checked live first (`podman 4.9.3`): a bare `podman create --quiet
//! <not-yet-present image>` prints only the resulting container id --
//! none of the usual `Trying to pull ...`/`Copying blob ...`/`Writing
//! manifest ...` lines a non-quiet pull always shows. `ociman`'s own
//! pull progress is a spinner rather than podman's own static lines,
//! and (like `ociman_tls_verify.rs`'s own `pull_quiet_still_pulls_
//! correctly`) that spinner only ever draws to stderr and is already
//! automatically hidden whenever stderr isn't a real terminal (true
//! of this whole automated suite) -- so there's no separately
//! observable output difference to assert on here; what *is* real and
//! checkable is that the flag is accepted and the real pull-then-
//! extract(-then-run) it performs is still entirely correct.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::Command;
use std::thread;

use oci_spec_types::image::{
    ContainerConfig, Descriptor, ImageConfig, ImageManifest, MEDIA_TYPE_IMAGE_CONFIG,
    MEDIA_TYPE_IMAGE_LAYER_GZIP, MEDIA_TYPE_IMAGE_MANIFEST, RootFs,
};

use oci_tools_tests::{bin_path, busybox_path};

fn ociman(storage_root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
        .env_remove("OCI_TOOLS_LOG")
        .args(args)
        .output()
        .expect("failed to spawn ociman")
}

/// Same minimal anonymous plain-HTTP/1.1 mock registry shape
/// `ociman_tls_verify.rs`'s own `MockRegistry` already establishes.
struct MockRegistry {
    addr: std::net::SocketAddr,
}

impl MockRegistry {
    fn start(routes: HashMap<String, (&'static str, Vec<u8>)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            while let Ok((stream, _)) = listener.accept() {
                Self::handle(stream, &routes);
            }
        });
        MockRegistry { addr }
    }

    fn handle(mut stream: TcpStream, routes: &HashMap<String, (&'static str, Vec<u8>)>) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request_line = String::new();
        reader.read_line(&mut request_line).unwrap();
        let path = request_line
            .split_whitespace()
            .nth(1)
            .unwrap_or("")
            .to_string();
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line.trim().is_empty() {
                break;
            }
        }
        match routes.get(&path) {
            Some((content_type, body)) => {
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(header.as_bytes()).unwrap();
                stream.write_all(body).unwrap();
            }
            None => {
                let resp =
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                stream.write_all(resp.as_bytes()).unwrap();
            }
        }
    }
}

/// A real, single-layer image's own manifest/config/blob route table,
/// with a genuinely real, extractable gzip tar layer (`busybox` plus
/// symlinked applets) -- the same real bytes `oci_tools_tests::
/// seed_image_with_files_and_compression` builds for the fully-local
/// case, constructed by hand here since that helper writes straight
/// into a `Store` rather than returning the raw bytes a mock registry
/// route table needs.
fn start_mock_with_a_real_extractable_image(busybox: &Path, applets: &[&str]) -> MockRegistry {
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
    let tar_bytes = builder.into_inner().unwrap();
    let diff_id = oci_spec_types::digest::sha256(&tar_bytes);

    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&tar_bytes).unwrap();
    let layer_bytes = encoder.finish().unwrap();
    let layer_digest = oci_spec_types::digest::sha256(&layer_bytes);

    let image_config = ImageConfig {
        architecture: Some(std::env::consts::ARCH.to_string()),
        os: Some("linux".to_string()),
        config: Some(ContainerConfig::default()),
        rootfs: RootFs {
            kind: "layers".to_string(),
            diff_ids: vec![diff_id],
        },
        ..Default::default()
    };
    let config_bytes = serde_json::to_vec(&image_config).unwrap();
    let config_digest = oci_spec_types::digest::sha256(&config_bytes);

    let manifest = ImageManifest {
        schema_version: 2,
        media_type: Some(MEDIA_TYPE_IMAGE_MANIFEST.to_string()),
        config: Descriptor {
            media_type: MEDIA_TYPE_IMAGE_CONFIG.to_string(),
            digest: config_digest.clone(),
            size: config_bytes.len() as u64,
            urls: vec![],
            annotations: Default::default(),
            platform: None,
        },
        layers: vec![Descriptor {
            media_type: MEDIA_TYPE_IMAGE_LAYER_GZIP.to_string(),
            digest: layer_digest.clone(),
            size: layer_bytes.len() as u64,
            urls: vec![],
            annotations: Default::default(),
            platform: None,
        }],
        annotations: Default::default(),
    };
    let manifest_bytes = serde_json::to_vec(&manifest).unwrap();

    let mut routes = HashMap::new();
    routes.insert(
        "/v2/testrepo/manifests/latest".to_string(),
        (MEDIA_TYPE_IMAGE_MANIFEST, manifest_bytes),
    );
    routes.insert(
        format!("/v2/testrepo/blobs/{config_digest}"),
        ("application/octet-stream", config_bytes),
    );
    routes.insert(
        format!("/v2/testrepo/blobs/{layer_digest}"),
        ("application/octet-stream", layer_bytes),
    );
    MockRegistry::start(routes)
}

/// `ociman create --quiet` against an image not yet present locally
/// still resolves, pulls, and extracts it correctly -- the container
/// lands in a real `created` state, exactly as without `--quiet`.
#[test]
fn create_quiet_still_pulls_and_creates_correctly() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let mock = start_mock_with_a_real_extractable_image(&busybox, &["sh", "true"]);
    let storage_dir = tempfile::tempdir().unwrap();

    let create = ociman(
        storage_dir.path(),
        &[
            "create",
            "--quiet",
            "--tls-verify=false",
            &format!("{}/testrepo:latest", mock.addr),
            "true",
        ],
    );
    assert!(
        create.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&create.stderr)
    );
    // Real `podman create --quiet` prints only the container id on
    // success -- matching that here too.
    let id = String::from_utf8_lossy(&create.stdout).trim().to_string();
    assert!(!id.is_empty(), "{create:?}");

    let inspect = ociman(storage_dir.path(), &["inspect", &id, "--json"]);
    assert!(inspect.status.success());
    let json: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert_eq!(json["status"], "created", "{json:?}");
}

/// Same as [`create_quiet_still_pulls_and_creates_correctly`], but for
/// `ociman run --quiet`, which additionally actually launches the
/// container -- proving `--quiet` doesn't affect the run itself, only
/// the pull-progress spinner beforehand.
#[test]
fn run_quiet_still_pulls_and_runs_correctly() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let mock = start_mock_with_a_real_extractable_image(&busybox, &["sh", "true"]);
    let storage_dir = tempfile::tempdir().unwrap();

    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "--rm",
            "--quiet",
            "--tls-verify=false",
            &format!("{}/testrepo:latest", mock.addr),
            "true",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    // Real `podman run --quiet` prints nothing at all besides the
    // container's own output (here: nothing, `true` prints nothing) --
    // matching that here too, no stray pull-progress lines/spinner
    // artifacts on stdout.
    assert!(
        String::from_utf8_lossy(&run.stdout).trim().is_empty(),
        "{run:?}"
    );
}
