//! `ociman run --pull`/`ociman build --pull`: exercises the actual
//! built `ociman` binary's own `--pull` flag (`always`/`missing`/
//! `never`), verified against real, checked-directly behavior (see
//! `docs/design/0140`): a real installed `podman run --pull`/`podman
//! build --pull` were both used first to confirm exactly when each
//! policy does or doesn't touch the network — `missing` (the
//! default) and `never` both skip a registry round trip entirely once
//! something is already resolved locally; `always` never skips one,
//! confirmed here two different ways: a real, counted mock-registry
//! request (for `ociman build`, which can pull a metadata-only base
//! image without ever needing to extract a real rootfs from it), and
//! an intentionally unreachable host (for `ociman run`, which always
//! needs a real, extractable rootfs to launch into, so a real mock
//! registry serving a placeholder, non-extractable layer isn't usable
//! here) — `always` failing specifically because it *tried* to reach
//! that unreachable host, while `missing`/`never` succeed against the
//! exact same unreachable-host reference because they never attempt
//! to either.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

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

fn write_containerfile(dir: &Path, contents: &str) {
    std::fs::write(dir.join("Containerfile"), contents).unwrap();
}

/// A minimal anonymous (no-auth) plain-HTTP/1.1 mock registry serving
/// a real, single-layer image's own manifest/config/blob from a fixed
/// route table (same pattern `ociman_tls_verify.rs`'s own
/// `MockRegistry` already establishes), extended here with a real,
/// shared counter of how many times its own manifest route was
/// actually requested — the direct, positive proof `--pull always`
/// needs that a real registry round trip genuinely happened, not just
/// that the overall command succeeded.
struct MockRegistry {
    addr: std::net::SocketAddr,
    manifest_requests: Arc<AtomicUsize>,
}

impl MockRegistry {
    fn start(routes: HashMap<String, (&'static str, Vec<u8>)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let manifest_requests = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&manifest_requests);
        thread::spawn(move || {
            while let Ok((stream, _)) = listener.accept() {
                Self::handle(stream, &routes, &counter);
            }
        });
        MockRegistry {
            addr,
            manifest_requests,
        }
    }

    fn handle(
        mut stream: TcpStream,
        routes: &HashMap<String, (&'static str, Vec<u8>)>,
        counter: &Arc<AtomicUsize>,
    ) {
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
        if path.contains("/manifests/") {
            counter.fetch_add(1, Ordering::SeqCst);
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

/// An address nothing is ever listening on in this environment
/// (loopback, a low, privileged port real-world services never bind
/// rootless) — connecting to it fails fast (a real, immediate
/// "connection refused", not a slow timeout), used here purely to
/// prove whether `ociman run --pull <policy>` ever actually *attempts*
/// a real network connection at all for an image already resolved
/// locally.
const UNREACHABLE_HOST: &str = "127.0.0.1:1";

#[test]
fn run_pull_missing_default_skips_the_network_when_already_present() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    let reference = format!("{UNREACHABLE_HOST}/testrepo:latest");
    seed_image(
        &store,
        &reference,
        &busybox,
        &["sh", "true"],
        ContainerConfig::default(),
    );

    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "--rm",
            "--pull",
            "missing",
            &reference,
            "--",
            "/bin/true",
        ],
    );
    assert!(
        run.status.success(),
        "status: {:?} stdout: {} stderr: {}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn run_pull_never_skips_the_network_when_already_present() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    let reference = format!("{UNREACHABLE_HOST}/testrepo:latest");
    seed_image(
        &store,
        &reference,
        &busybox,
        &["sh", "true"],
        ContainerConfig::default(),
    );

    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "--rm",
            "--pull",
            "never",
            &reference,
            "--",
            "/bin/true",
        ],
    );
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn run_pull_never_fails_clearly_when_absent() {
    let storage_dir = tempfile::tempdir().unwrap();
    let reference = format!("{UNREACHABLE_HOST}/testrepo:latest");

    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "--rm",
            "--pull",
            "never",
            &reference,
            "--",
            "/bin/true",
        ],
    );
    assert!(!run.status.success());
    assert!(
        String::from_utf8_lossy(&run.stderr).contains("no such image in local storage"),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
}

/// The real, positive distinction from the two tests above: `always`
/// really does attempt a real network connection even when the exact
/// same reference is already fully resolved locally -- proven here by
/// it *failing* against an address nothing is listening on, where
/// `missing`/`never` both succeed unchanged.
#[test]
fn run_pull_always_still_attempts_the_network_even_when_already_present() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    let reference = format!("{UNREACHABLE_HOST}/testrepo:latest");
    seed_image(
        &store,
        &reference,
        &busybox,
        &["sh", "true"],
        ContainerConfig::default(),
    );

    let run = ociman(
        storage_dir.path(),
        &[
            "run",
            "--rm",
            "--pull",
            "always",
            &reference,
            "--",
            "/bin/true",
        ],
    );
    assert!(!run.status.success());
}

/// `--pull` with no explicit value at all is a real, immediate CLI
/// parse error for `ociman run` -- confirmed directly against real
/// `podman run --pull` (no `default-missing-value` the way `ociman
/// build --pull`'s identical flag has).
#[test]
fn run_bare_pull_flag_with_no_value_is_a_clear_cli_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let run = ociman(
        storage_dir.path(),
        &["run", "--rm", "--pull", "busybox:latest"],
    );
    assert!(!run.status.success());
}

#[test]
fn build_pull_missing_default_skips_a_real_registry_fetch_when_already_present() {
    let mock = start_mock_with_a_real_image();
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    let base_reference = format!("{}/testrepo:latest", mock.addr);
    // Seeded directly -- never actually pulled from the mock at all.
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    seed_image(
        &store,
        &base_reference,
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        &format!("FROM {base_reference}\nLABEL pull=policy-test\n"),
    );
    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "pull-policy-test/missing-result:latest",
            "--pull",
            "missing",
            "--tls-verify=false",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    assert_eq!(mock.manifest_requests.load(Ordering::SeqCst), 0);
}

#[test]
fn build_pull_always_makes_a_real_registry_fetch_even_when_already_present() {
    let mock = start_mock_with_a_real_image();
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    let base_reference = format!("{}/testrepo:latest", mock.addr);
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    seed_image(
        &store,
        &base_reference,
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        &format!("FROM {base_reference}\nLABEL pull=policy-test\n"),
    );
    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "pull-policy-test/always-result:latest",
            "--pull",
            "always",
            "--tls-verify=false",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    assert!(mock.manifest_requests.load(Ordering::SeqCst) >= 1);
}

/// A bare `--pull` (no explicit value) really does default to
/// `always` for `ociman build` -- confirmed directly against real
/// `podman build --pull` (no value) -- unlike `ociman run`'s own
/// identical flag.
#[test]
fn build_bare_pull_flag_with_no_value_defaults_to_always() {
    let mock = start_mock_with_a_real_image();
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    let base_reference = format!("{}/testrepo:latest", mock.addr);
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    seed_image(
        &store,
        &base_reference,
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let context_dir = tempfile::tempdir().unwrap();
    write_containerfile(
        context_dir.path(),
        &format!("FROM {base_reference}\nLABEL pull=policy-test\n"),
    );
    let build = ociman(
        storage_dir.path(),
        &[
            "build",
            context_dir.path().to_str().unwrap(),
            "-t",
            "pull-policy-test/bare-result:latest",
            "--pull",
            "--tls-verify=false",
        ],
    );
    assert!(
        build.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    assert!(mock.manifest_requests.load(Ordering::SeqCst) >= 1);
}
