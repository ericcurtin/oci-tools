//! `ociman pull`/`run`/`create --platform` (0307): a real, fully
//! offline plain-HTTP mock registry serves a genuine multi-platform
//! `ImageIndex` (two child manifests, `linux/arm64` and
//! `linux/amd64`), proving `--platform` actually steers which child
//! manifest gets fetched -- not just that the flag parses. Same
//! `MockRegistry` shape `ociman_pull_policy.rs`/`ociman_tls_verify.rs`
//! already establish, extended here to serve a real index instead of
//! a single manifest.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::Command;
use std::thread;

use flate2::Compression;
use flate2::write::GzEncoder;

use oci_spec_types::image::{
    Descriptor, ImageConfig, ImageIndex, ImageManifest, MEDIA_TYPE_IMAGE_CONFIG,
    MEDIA_TYPE_IMAGE_INDEX, MEDIA_TYPE_IMAGE_LAYER_GZIP, MEDIA_TYPE_IMAGE_MANIFEST, Platform,
    RootFs,
};
use oci_tools_tests::bin_path;

fn ociman(storage_root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin_path("ociman"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
        .env_remove("OCI_TOOLS_LOG")
        .args(args)
        .output()
        .expect("failed to spawn ociman")
}

type Routes = HashMap<String, (&'static str, Vec<u8>)>;

struct MockRegistry {
    addr: std::net::SocketAddr,
}

impl MockRegistry {
    fn start(routes: Routes) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            while let Ok((stream, _)) = listener.accept() {
                Self::handle(stream, &routes);
            }
        });
        MockRegistry { addr }
    }

    fn handle(mut stream: TcpStream, routes: &Routes) {
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

/// One platform's own single-layer manifest, config, and layer blob --
/// distinct `layer_content` per platform gives each a genuinely
/// different manifest digest, so which one actually got fetched is
/// unambiguous.
struct PlatformImage {
    manifest_digest: oci_spec_types::Digest,
    manifest_bytes: Vec<u8>,
    config_digest: oci_spec_types::Digest,
    config_bytes: Vec<u8>,
    layer_digest: oci_spec_types::Digest,
    layer_bytes: Vec<u8>,
}

fn build_platform_image(architecture: &str, marker_file_content: &[u8]) -> PlatformImage {
    // A real, minimal tar archive (one small regular file) -- distinct
    // `marker_file_content` per platform gives each a genuinely
    // different layer (and therefore manifest) digest.
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(marker_file_content.len() as u64);
    header.set_mode(0o644);
    header.set_entry_type(tar::EntryType::Regular);
    builder
        .append_data(&mut header, "platform-marker.txt", marker_file_content)
        .unwrap();
    let layer_tar_content = builder.into_inner().unwrap();
    let diff_id = oci_spec_types::digest::sha256(&layer_tar_content);

    let config = ImageConfig {
        architecture: Some(architecture.to_string()),
        os: Some("linux".to_string()),
        rootfs: RootFs {
            kind: "layers".to_string(),
            diff_ids: vec![diff_id],
        },
        ..Default::default()
    };
    let config_bytes = serde_json::to_vec(&config).unwrap();
    let config_digest = oci_spec_types::digest::sha256(&config_bytes);

    // A real gzip stream around the real tar -- `ociman create`
    // actually extracts this layer into a real rootfs, unlike
    // `ociman pull` alone, which never decompresses anything at all.
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&layer_tar_content).unwrap();
    let layer_bytes = encoder.finish().unwrap();
    let layer_digest = oci_spec_types::digest::sha256(&layer_bytes);

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
    let manifest_digest = oci_spec_types::digest::sha256(&manifest_bytes);

    PlatformImage {
        manifest_digest,
        manifest_bytes,
        config_digest,
        config_bytes,
        layer_digest,
        layer_bytes,
    }
}

/// A real registry serving `latest` as a genuine two-platform
/// `ImageIndex` (`linux/arm64`/`linux/amd64`), each child a real,
/// independently fetchable single-platform manifest.
fn start_multi_platform_mock() -> (MockRegistry, PlatformImage, PlatformImage) {
    let arm64 = build_platform_image("arm64", b"arm64 layer content");
    let amd64 = build_platform_image("amd64", b"amd64 layer content");

    let index = ImageIndex {
        schema_version: 2,
        media_type: Some(MEDIA_TYPE_IMAGE_INDEX.to_string()),
        manifests: vec![
            Descriptor {
                media_type: MEDIA_TYPE_IMAGE_MANIFEST.to_string(),
                digest: arm64.manifest_digest.clone(),
                size: arm64.manifest_bytes.len() as u64,
                urls: vec![],
                annotations: Default::default(),
                platform: Some(Platform {
                    os: "linux".to_string(),
                    architecture: "arm64".to_string(),
                    // Matching real-world published multi-arch
                    // images (e.g. real Docker Hub manifest lists),
                    // which declare `v8` for their own `arm64`
                    // entries -- needed so a bare `ociman pull` with
                    // no explicit `--platform` (using `Platform::
                    // host()`, which reports `variant: Some("v8")`
                    // on a real aarch64 host) actually matches this
                    // entry too, not just an explicit, variant-less
                    // `--platform linux/arm64` request.
                    variant: Some("v8".to_string()),
                    os_version: None,
                }),
            },
            Descriptor {
                media_type: MEDIA_TYPE_IMAGE_MANIFEST.to_string(),
                digest: amd64.manifest_digest.clone(),
                size: amd64.manifest_bytes.len() as u64,
                urls: vec![],
                annotations: Default::default(),
                platform: Some(Platform {
                    os: "linux".to_string(),
                    architecture: "amd64".to_string(),
                    variant: None,
                    os_version: None,
                }),
            },
        ],
        annotations: Default::default(),
    };
    let index_bytes = serde_json::to_vec(&index).unwrap();

    let mut routes: Routes = HashMap::new();
    routes.insert(
        "/v2/testrepo/manifests/latest".to_string(),
        (MEDIA_TYPE_IMAGE_INDEX, index_bytes),
    );
    for image in [&arm64, &amd64] {
        routes.insert(
            format!("/v2/testrepo/manifests/{}", image.manifest_digest),
            (MEDIA_TYPE_IMAGE_MANIFEST, image.manifest_bytes.clone()),
        );
        routes.insert(
            format!("/v2/testrepo/blobs/{}", image.config_digest),
            ("application/octet-stream", image.config_bytes.clone()),
        );
        routes.insert(
            format!("/v2/testrepo/blobs/{}", image.layer_digest),
            ("application/octet-stream", image.layer_bytes.clone()),
        );
    }

    let mock = MockRegistry::start(routes);
    (mock, arm64, amd64)
}

/// `ociman pull --platform linux/amd64` fetches the `amd64` child
/// manifest out of a real multi-platform index, not the `arm64` one a
/// bare `ociman pull` (matching this test's own host-independent
/// default-platform behavior isn't asserted here) would otherwise
/// resolve to `Manifest::Index::select`'s own first match.
#[test]
fn pull_platform_selects_the_matching_child_manifest_from_a_real_index() {
    let (mock, arm64, amd64) = start_multi_platform_mock();
    let storage_dir = tempfile::tempdir().unwrap();

    let reference = format!("{}/testrepo:latest", mock.addr);
    let pull = ociman(
        storage_dir.path(),
        &[
            "pull",
            "--tls-verify=false",
            "--platform",
            "linux/amd64",
            &reference,
        ],
    );
    assert!(
        pull.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&pull.stderr)
    );
    let digest = String::from_utf8_lossy(&pull.stdout).trim().to_string();
    assert_eq!(digest, amd64.manifest_digest.to_string());
    assert_ne!(digest, arm64.manifest_digest.to_string());

    let inspect = ociman(storage_dir.path(), &["inspect", &reference]);
    assert!(inspect.status.success());
    let view: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert_eq!(view["architecture"], "amd64");
}

/// The other platform in the same index: `--platform linux/arm64`
/// fetches the `arm64` child instead, proving this isn't a hardcoded
/// "always the second entry" test artifact.
#[test]
fn pull_platform_selects_the_other_matching_child_manifest() {
    let (mock, arm64, amd64) = start_multi_platform_mock();
    let storage_dir = tempfile::tempdir().unwrap();

    let reference = format!("{}/testrepo:latest", mock.addr);
    let pull = ociman(
        storage_dir.path(),
        &[
            "pull",
            "--tls-verify=false",
            "--platform",
            "linux/arm64",
            &reference,
        ],
    );
    assert!(
        pull.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&pull.stderr)
    );
    let digest = String::from_utf8_lossy(&pull.stdout).trim().to_string();
    assert_eq!(digest, arm64.manifest_digest.to_string());
    assert_ne!(digest, amd64.manifest_digest.to_string());
}

/// A pull with no `--platform` at all still works exactly as before
/// this flag existed -- resolving to whichever platform this real
/// test host actually is (never an error), matching the pre-existing,
/// unconditional `Platform::host()` default.
#[test]
fn pull_with_no_platform_flag_still_resolves_to_a_real_platform() {
    let (mock, arm64, amd64) = start_multi_platform_mock();
    let storage_dir = tempfile::tempdir().unwrap();

    let reference = format!("{}/testrepo:latest", mock.addr);
    let pull = ociman(
        storage_dir.path(),
        &["pull", "--tls-verify=false", &reference],
    );
    assert!(
        pull.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&pull.stderr)
    );
    let digest = String::from_utf8_lossy(&pull.stdout).trim().to_string();
    // Whichever of the two this real host's own architecture actually
    // matches -- either is a legitimate outcome, unlike an error.
    assert!(
        digest == arm64.manifest_digest.to_string() || digest == amd64.manifest_digest.to_string(),
        "expected the host's own real platform to resolve to one of the two real children: {digest}"
    );
}

/// An invalid `--platform` value is a real, clear CLI-input error,
/// matching `ociman build --platform`'s own identical parser.
#[test]
fn pull_platform_rejects_an_invalid_value() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ociman(
        storage_dir.path(),
        &[
            "pull",
            "--platform",
            "bogus",
            "docker.io/library/does-not-matter:latest",
        ],
    );
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("missing an architecture"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `ociman create --platform` threads the same selection through
/// `prepare_container`'s own pull path -- verified by inspecting the
/// created container's own resolved image, not just that `create`
/// itself succeeded.
#[test]
fn create_platform_selects_the_matching_child_manifest() {
    let (mock, _arm64, _amd64) = start_multi_platform_mock();
    let storage_dir = tempfile::tempdir().unwrap();

    let reference = format!("{}/testrepo:latest", mock.addr);
    let create = ociman(
        storage_dir.path(),
        &[
            "create",
            "--tls-verify=false",
            "--platform",
            "linux/amd64",
            &reference,
            "true",
        ],
    );
    assert!(
        create.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&create.stderr)
    );

    let inspect = ociman(storage_dir.path(), &["inspect", &reference]);
    assert!(inspect.status.success());
    let view: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert_eq!(view["architecture"], "amd64");
}
