//! `ociman pull`/`ociman push --tls-verify`: exercises the actual
//! built `ociman` binary's own `--tls-verify` flag against a real,
//! local, anonymous (no-auth) plain-HTTP mock registry -- `oci_
//! registry`'s own pull/push mechanism already has its own thorough
//! mock-registry test coverage (`crates/oci-registry/src/{pull,
//! push}.rs`), including a real, manually-verified end-to-end round
//! trip against a real local `registry:2` instance (`docs/design/
//! 0127`); this is a CLI-surface test on top of it, proving the
//! `--tls-verify=false` flag actually reaches `oci_registry::Client`'s
//! own `insecure_hosts`.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::Command;
use std::thread;

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

/// A minimal anonymous (no-auth) plain-HTTP/1.1 mock registry serving
/// a real, single-layer image's own manifest/config/blob from a fixed
/// route table -- same pattern `oci_registry::pull`'s own tests
/// already established, reused here at the CLI level specifically to
/// prove `--tls-verify=false` actually reaches the registry client.
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

fn start_mock_with_a_real_image() -> MockRegistry {
    let config = oci_spec_types::image::ImageConfig {
        architecture: Some("arm64".to_string()),
        os: Some("linux".to_string()),
        rootfs: oci_spec_types::image::RootFs {
            kind: "layers".to_string(),
            diff_ids: vec![],
        },
        ..Default::default()
    };
    let config_bytes = serde_json::to_vec(&config).unwrap();
    let config_digest = oci_spec_types::digest::sha256(&config_bytes);

    let layer_bytes = b"a fake layer tarball".to_vec();
    let layer_digest = oci_spec_types::digest::sha256(&layer_bytes);

    let manifest = oci_spec_types::image::ImageManifest {
        schema_version: 2,
        media_type: Some(oci_spec_types::image::MEDIA_TYPE_IMAGE_MANIFEST.to_string()),
        config: oci_spec_types::image::Descriptor {
            media_type: oci_spec_types::image::MEDIA_TYPE_IMAGE_CONFIG.to_string(),
            digest: config_digest.clone(),
            size: config_bytes.len() as u64,
            urls: vec![],
            annotations: Default::default(),
            platform: None,
        },
        layers: vec![oci_spec_types::image::Descriptor {
            media_type: oci_spec_types::image::MEDIA_TYPE_IMAGE_LAYER_GZIP.to_string(),
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
        (
            oci_spec_types::image::MEDIA_TYPE_IMAGE_MANIFEST,
            manifest_bytes,
        ),
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

/// Same shape as [`start_mock_with_a_real_image`], except its one
/// layer is a *real* tar+gzip archive containing a single regular
/// file — needed by `ociman build`'s own `FROM`/`COPY --from=` path,
/// which (unlike plain `ociman pull`) actually extracts a layer into
/// a real rootfs cache directory whenever the stage runs a `RUN`/
/// `COPY` or is itself a `COPY --from=` source (see `ensure_cached` in
/// `crates/oci-store/src/rootfs_cache.rs`); a non-tar placeholder blob
/// like `start_mock_with_a_real_image`'s would fail to extract.
fn start_mock_with_a_real_extractable_image() -> MockRegistry {
    let mut tar_builder = tar::Builder::new(Vec::new());
    let contents = b"from the external image\n";
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(contents.len() as u64);
    header.set_mode(0o644);
    tar_builder
        .append_data(&mut header, "marker.txt", &contents[..])
        .unwrap();
    let tar_bytes = tar_builder.into_inner().unwrap();
    let diff_id = oci_spec_types::digest::sha256(&tar_bytes);

    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&tar_bytes).unwrap();
    let layer_bytes = encoder.finish().unwrap();
    let layer_digest = oci_spec_types::digest::sha256(&layer_bytes);

    let config = oci_spec_types::image::ImageConfig {
        architecture: Some(std::env::consts::ARCH.to_string()),
        os: Some("linux".to_string()),
        rootfs: oci_spec_types::image::RootFs {
            kind: "layers".to_string(),
            diff_ids: vec![diff_id],
        },
        ..Default::default()
    };
    let config_bytes = serde_json::to_vec(&config).unwrap();
    let config_digest = oci_spec_types::digest::sha256(&config_bytes);

    let manifest = oci_spec_types::image::ImageManifest {
        schema_version: 2,
        media_type: Some(oci_spec_types::image::MEDIA_TYPE_IMAGE_MANIFEST.to_string()),
        config: oci_spec_types::image::Descriptor {
            media_type: oci_spec_types::image::MEDIA_TYPE_IMAGE_CONFIG.to_string(),
            digest: config_digest.clone(),
            size: config_bytes.len() as u64,
            urls: vec![],
            annotations: Default::default(),
            platform: None,
        },
        layers: vec![oci_spec_types::image::Descriptor {
            media_type: oci_spec_types::image::MEDIA_TYPE_IMAGE_LAYER_GZIP.to_string(),
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
        (
            oci_spec_types::image::MEDIA_TYPE_IMAGE_MANIFEST,
            manifest_bytes,
        ),
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

#[test]
fn pull_with_tls_verify_false_succeeds_against_a_real_plain_http_registry() {
    let mock = start_mock_with_a_real_image();
    let storage_dir = tempfile::tempdir().unwrap();

    let pull = ociman(
        storage_dir.path(),
        &[
            "pull",
            "--tls-verify=false",
            &format!("{}/testrepo:latest", mock.addr),
        ],
    );
    assert!(
        pull.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&pull.stderr)
    );
}

#[test]
fn pull_without_tls_verify_false_refuses_plain_http_by_default() {
    let mock = start_mock_with_a_real_image();
    let storage_dir = tempfile::tempdir().unwrap();

    // Default (`--tls-verify` omitted, real `true`): attempts HTTPS
    // against a registry that only ever speaks plain HTTP here -- a
    // real, clear failure, not a silent fallback to HTTP.
    let pull = ociman(
        storage_dir.path(),
        &["pull", &format!("{}/testrepo:latest", mock.addr)],
    );
    assert!(!pull.status.success());
}

/// A minimal anonymous, plain-HTTP/1.1 mock registry implementing just
/// enough of the real push protocol (`HEAD`/`POST`/`PUT`) to exercise
/// `ociman push --tls-verify=false` end to end at the CLI level — the
/// same shape `oci_registry::push`'s own mock-registry tests already
/// use at the library level, reused here specifically to prove the
/// flag reaches the client on the push side too, independently of
/// pull.
struct MockPushRegistry {
    addr: std::net::SocketAddr,
}

impl MockPushRegistry {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            while let Ok((stream, _)) = listener.accept() {
                Self::handle(stream);
            }
        });
        MockPushRegistry { addr }
    }

    fn handle(mut stream: TcpStream) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request_line = String::new();
        reader.read_line(&mut request_line).unwrap();
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or("").to_string();
        let path = parts.next().unwrap_or("").to_string();

        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line.trim().is_empty() {
                break;
            }
            if let Some(idx) = line.to_ascii_lowercase().find("content-length:") {
                content_length = line[idx + "content-length:".len()..]
                    .trim()
                    .parse()
                    .unwrap_or(0);
            }
        }
        let mut body = vec![0u8; content_length];
        std::io::Read::read_exact(&mut reader, &mut body).unwrap();

        let write_status = |stream: &mut TcpStream, status: u16, extra_headers: &str| {
            let text = match status {
                201 => "Created",
                202 => "Accepted",
                404 => "Not Found",
                _ => "Error",
            };
            let resp = format!(
                "HTTP/1.1 {status} {text}\r\n{extra_headers}Content-Length: 0\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(resp.as_bytes()).unwrap();
        };

        if method == "HEAD" && path.contains("/blobs/") {
            write_status(&mut stream, 404, ""); // nothing exists yet; upload everything
        } else if method == "POST" && path.ends_with("/blobs/uploads/") {
            let repo = path
                .strip_prefix("/v2/")
                .unwrap()
                .strip_suffix("/blobs/uploads/")
                .unwrap();
            write_status(
                &mut stream,
                202,
                &format!("Location: /v2/{repo}/blobs/uploads/test-upload-id\r\n"),
            );
        } else if method == "PUT" {
            write_status(&mut stream, 201, "");
        } else {
            write_status(&mut stream, 404, "");
        }
    }
}

#[test]
fn push_with_tls_verify_false_reaches_a_real_plain_http_registry() {
    let source_mock = start_mock_with_a_real_image();
    let storage_dir = tempfile::tempdir().unwrap();
    let pull = ociman(
        storage_dir.path(),
        &[
            "pull",
            "--tls-verify=false",
            &format!("{}/testrepo:latest", source_mock.addr),
        ],
    );
    assert!(pull.status.success());

    let dest_mock = MockPushRegistry::start();
    let tag = ociman(
        storage_dir.path(),
        &[
            "tag",
            &format!("{}/testrepo:latest", source_mock.addr),
            &format!("{}/testrepo:latest", dest_mock.addr),
        ],
    );
    assert!(tag.status.success());

    let push = ociman(
        storage_dir.path(),
        &[
            "push",
            "--tls-verify=false",
            &format!("{}/testrepo:latest", dest_mock.addr),
        ],
    );
    assert!(
        push.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&push.stderr)
    );
}

#[test]
fn push_without_tls_verify_false_refuses_plain_http_by_default() {
    let source_mock = start_mock_with_a_real_image();
    let storage_dir = tempfile::tempdir().unwrap();
    assert!(
        ociman(
            storage_dir.path(),
            &[
                "pull",
                "--tls-verify=false",
                &format!("{}/testrepo:latest", source_mock.addr),
            ],
        )
        .status
        .success()
    );

    let dest_mock = MockPushRegistry::start();
    assert!(
        ociman(
            storage_dir.path(),
            &[
                "tag",
                &format!("{}/testrepo:latest", source_mock.addr),
                &format!("{}/testrepo:latest", dest_mock.addr),
            ],
        )
        .status
        .success()
    );

    // Default (`--tls-verify` omitted): attempts HTTPS against a
    // registry that only ever speaks plain HTTP here.
    let push = ociman(
        storage_dir.path(),
        &["push", &format!("{}/testrepo:latest", dest_mock.addr)],
    );
    assert!(!push.status.success());
}

fn write_containerfile(dir: &Path, contents: &str) {
    std::fs::write(dir.join("Containerfile"), contents).unwrap();
}

/// `ociman build`'s own `FROM <registry>/...` path shares `resolve_or_
/// pull` with `ociman pull`/`ociman run` (see `docs/design/0129`), so
/// this proves `--tls-verify` actually reaches it too, not just the
/// standalone `pull`/`push`/`run` commands already covered above. No
/// `RUN`/`COPY` in this stage, so nothing ever extracts the mock's
/// layer — a metadata-only instruction (`LABEL`) is enough to force a
/// real build, matching `ociman_build.rs`'s own metadata-only test.
#[test]
fn build_from_with_tls_verify_false_pulls_the_base_image_over_plain_http() {
    let mock = start_mock_with_a_real_image();
    let storage_dir = tempfile::tempdir().unwrap();
    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        &format!(
            "FROM {}/testrepo:latest\nLABEL pulled=over-plain-http\n",
            mock.addr
        ),
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "tls-verify-test/from-result:latest",
            "--tls-verify=false",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );
}

#[test]
fn build_from_without_tls_verify_false_refuses_plain_http_by_default() {
    let mock = start_mock_with_a_real_image();
    let storage_dir = tempfile::tempdir().unwrap();
    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        &format!(
            "FROM {}/testrepo:latest\nLABEL pulled=over-plain-http\n",
            mock.addr
        ),
    );

    // Default (`--tls-verify` omitted): attempts HTTPS against a
    // registry that only ever speaks plain HTTP here.
    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "tls-verify-test/from-result:latest",
        ],
    );
    assert!(!build.status.success());
}

/// `COPY --from=<external-image>`'s own pull call site
/// (`external_image_source_root` in `bin/ociman/src/build.rs`) is a
/// second, separate `resolve_or_pull` call site from `FROM`'s —
/// exercised here specifically because it's the one that actually
/// extracts the pulled layer into a real rootfs cache directory
/// (`ensure_cached`), unlike the metadata-only `FROM` test above.
#[test]
fn build_copy_from_with_tls_verify_false_pulls_and_extracts_over_plain_http() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let mock = start_mock_with_a_real_extractable_image();
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "tls-verify-test/copyfrom-consumer-base:latest",
        &busybox,
        &["sh", "cat"],
        oci_spec_types::image::ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        &format!(
            "FROM tls-verify-test/copyfrom-consumer-base:latest\n\
             COPY --from={}/testrepo:latest /marker.txt /marker.txt\n",
            mock.addr
        ),
    );

    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "tls-verify-test/copyfrom-result:latest",
            "--tls-verify=false",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );
}

#[test]
fn build_copy_from_without_tls_verify_false_refuses_plain_http_by_default() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let mock = start_mock_with_a_real_extractable_image();
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "tls-verify-test/copyfrom-consumer-base:latest",
        &busybox,
        &["sh", "cat"],
        oci_spec_types::image::ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        &format!(
            "FROM tls-verify-test/copyfrom-consumer-base:latest\n\
             COPY --from={}/testrepo:latest /marker.txt /marker.txt\n",
            mock.addr
        ),
    );

    // Default (`--tls-verify` omitted): attempts HTTPS against a
    // registry that only ever speaks plain HTTP here.
    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "tls-verify-test/copyfrom-result:latest",
        ],
    );
    assert!(!build.status.success());
}
