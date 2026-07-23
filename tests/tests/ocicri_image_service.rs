//! `ocicri` `ImageService` integration tests: spawns the actual built
//! `ocicri` binary as a real server over a real Unix socket, pointed
//! at a real, seeded `oci_store::Store` (the same synthetic-but-
//! structurally-real image fixture `ociman_run.rs`/`ocibox_create.rs`
//! already use — a real busybox binary tarred and gzipped, a real
//! `ImageConfig`/`ImageManifest`, ingested exactly the way a real
//! registry pull would have left them), and calls `ListImages`/
//! `ImageStatus` via the exact same shared, generated `tonic` client
//! the server itself uses.

use std::path::Path;
use std::process::{Child, Command};
use std::time::Duration;

use oci_cri_types::image_service_client::ImageServiceClient;
use oci_cri_types::{ImageFilter, ImageSpec, ImageStatusRequest, ListImagesRequest};
use oci_spec_types::image::ContainerConfig;
use oci_store::Store;
use oci_tools_tests::{bin_path, busybox_path, seed_image, seed_image_with_files};

struct Server {
    child: Child,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_server(storage_root: &Path, socket_path: &Path) -> Server {
    let child = Command::new(bin_path("ocicri"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
        .env_remove("OCI_TOOLS_LOG")
        .args(["--listen", socket_path.to_str().unwrap()])
        .spawn()
        .expect("failed to spawn ocicri");
    Server { child }
}

fn wait_for_socket(socket_path: &Path) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !socket_path.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "ocicri never created its own socket at {}",
            socket_path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

async fn connect(socket_path: std::path::PathBuf) -> ImageServiceClient<tonic::transport::Channel> {
    let channel = tonic::transport::Endpoint::try_from("http://[::]:50051")
        .unwrap()
        .connect_with_connector(tower::service_fn(move |_: tonic::transport::Uri| {
            let socket_path = socket_path.clone();
            async move {
                let stream = tokio::net::UnixStream::connect(socket_path).await?;
                Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
            }
        }))
        .await
        .expect("failed to connect to ocicri's own real unix socket");
    ImageServiceClient::new(channel)
}

#[tokio::test]
async fn list_images_reports_every_seeded_image_with_real_size_and_tags() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ocicri-test/list-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let socket_dir = tempfile::tempdir().unwrap();
    let socket_path = socket_dir.path().join("ocicri.sock");
    let _server = spawn_server(storage_dir.path(), &socket_path);
    wait_for_socket(&socket_path);

    let mut client = connect(socket_path).await;
    let response = client
        .list_images(ListImagesRequest { filter: None })
        .await
        .expect("ListImages failed")
        .into_inner();

    assert_eq!(response.images.len(), 1, "{:?}", response.images);
    let image = &response.images[0];
    assert!(image.id.starts_with("sha256:"), "{}", image.id);
    assert_eq!(
        image.repo_tags,
        vec!["docker.io/ocicri-test/list-base:latest".to_string()]
    );
    assert!(image.size > 0, "a real image should have a nonzero size");
    assert!(!image.pinned);
}

#[tokio::test]
async fn list_images_with_a_filter_returns_only_the_matching_image() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    // Genuinely different content (an extra file unique to each), so
    // the two images get genuinely different manifest digests rather
    // than being the exact same real image under two different tags
    // (which `list_images`'s own digest-grouping would then correctly
    // report as one image with both tags -- not what this test is
    // checking).
    seed_image_with_files(
        &store,
        "ocicri-test/filter-a:latest",
        &busybox,
        &["sh"],
        &[("marker-a.txt", b"a")],
        ContainerConfig::default(),
    );
    seed_image_with_files(
        &store,
        "ocicri-test/filter-b:latest",
        &busybox,
        &["sh"],
        &[("marker-b.txt", b"b")],
        ContainerConfig::default(),
    );

    let socket_dir = tempfile::tempdir().unwrap();
    let socket_path = socket_dir.path().join("ocicri.sock");
    let _server = spawn_server(storage_dir.path(), &socket_path);
    wait_for_socket(&socket_path);

    let mut client = connect(socket_path).await;
    let response = client
        .list_images(ListImagesRequest {
            filter: Some(ImageFilter {
                image: Some(ImageSpec {
                    image: "ocicri-test/filter-a:latest".to_string(),
                    ..Default::default()
                }),
            }),
        })
        .await
        .expect("ListImages failed")
        .into_inner();

    assert_eq!(response.images.len(), 1, "{:?}", response.images);
    assert_eq!(
        response.images[0].repo_tags,
        vec!["docker.io/ocicri-test/filter-a:latest".to_string()]
    );
}

#[tokio::test]
async fn list_images_with_a_filter_matching_nothing_returns_an_empty_list_not_an_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let socket_dir = tempfile::tempdir().unwrap();
    let socket_path = socket_dir.path().join("ocicri.sock");
    let _server = spawn_server(storage_dir.path(), &socket_path);
    wait_for_socket(&socket_path);

    let mut client = connect(socket_path).await;
    let response = client
        .list_images(ListImagesRequest {
            filter: Some(ImageFilter {
                image: Some(ImageSpec {
                    image: "ocicri-test/does-not-exist:latest".to_string(),
                    ..Default::default()
                }),
            }),
        })
        .await
        .expect("ListImages should succeed even when the filter matches nothing")
        .into_inner();

    assert!(response.images.is_empty());
}

#[tokio::test]
async fn image_status_resolves_by_real_short_id_and_reports_a_real_size() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ocicri-test/status-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let socket_dir = tempfile::tempdir().unwrap();
    let socket_path = socket_dir.path().join("ocicri.sock");
    let _server = spawn_server(storage_dir.path(), &socket_path);
    wait_for_socket(&socket_path);

    let mut client = connect(socket_path).await;

    // Resolve first by the real tag to learn the real id, then again
    // by a short prefix of that same id -- confirming the shared
    // `oci_store::resolve_by_reference_or_id` primitive (the same one
    // `ociman inspect`/`rmi` already use) genuinely backs this RPC,
    // not just exact-tag lookups.
    let by_tag = client
        .image_status(ImageStatusRequest {
            image: Some(ImageSpec {
                image: "ocicri-test/status-base:latest".to_string(),
                ..Default::default()
            }),
            verbose: false,
        })
        .await
        .expect("ImageStatus failed")
        .into_inner();
    assert!(by_tag.info.is_empty(), "non-verbose should report no info");
    let image = by_tag.image.expect("image should resolve by tag");
    assert!(image.size > 0);

    let short_id = image.id.strip_prefix("sha256:").unwrap()[..12].to_string();
    let by_id = client
        .image_status(ImageStatusRequest {
            image: Some(ImageSpec {
                image: short_id,
                ..Default::default()
            }),
            verbose: false,
        })
        .await
        .expect("ImageStatus by short id failed")
        .into_inner();
    assert_eq!(by_id.image.unwrap().id, image.id);
}

#[tokio::test]
async fn image_status_verbose_reports_real_labels_in_the_info_map() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    let mut labels = std::collections::BTreeMap::new();
    labels.insert("maintainer".to_string(), "someone@example.com".to_string());
    seed_image(
        &store,
        "ocicri-test/verbose-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig {
            labels,
            ..Default::default()
        },
    );

    let socket_dir = tempfile::tempdir().unwrap();
    let socket_path = socket_dir.path().join("ocicri.sock");
    let _server = spawn_server(storage_dir.path(), &socket_path);
    wait_for_socket(&socket_path);

    let mut client = connect(socket_path).await;
    let response = client
        .image_status(ImageStatusRequest {
            image: Some(ImageSpec {
                image: "ocicri-test/verbose-base:latest".to_string(),
                ..Default::default()
            }),
            verbose: true,
        })
        .await
        .expect("ImageStatus failed")
        .into_inner();

    let info = response.info.get("info").expect("verbose info key");
    assert!(
        info.contains("maintainer") && info.contains("someone@example.com"),
        "{info}"
    );
}

#[tokio::test]
async fn image_status_of_an_unresolvable_image_is_an_empty_response_not_an_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let socket_dir = tempfile::tempdir().unwrap();
    let socket_path = socket_dir.path().join("ocicri.sock");
    let _server = spawn_server(storage_dir.path(), &socket_path);
    wait_for_socket(&socket_path);

    let mut client = connect(socket_path).await;
    let response = client
        .image_status(ImageStatusRequest {
            image: Some(ImageSpec {
                image: "ocicri-test/does-not-exist:latest".to_string(),
                ..Default::default()
            }),
            verbose: false,
        })
        .await
        .expect("ImageStatus of an unresolvable image should not be a gRPC error")
        .into_inner();

    assert!(response.image.is_none());
}

#[tokio::test]
async fn image_status_with_no_image_specified_is_a_real_invalid_argument_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let socket_dir = tempfile::tempdir().unwrap();
    let socket_path = socket_dir.path().join("ocicri.sock");
    let _server = spawn_server(storage_dir.path(), &socket_path);
    wait_for_socket(&socket_path);

    let mut client = connect(socket_path).await;
    let status = client
        .image_status(ImageStatusRequest {
            image: None,
            verbose: false,
        })
        .await
        .expect_err("no image specified should be a real error");

    assert_eq!(status.code(), tonic::Code::InvalidArgument);
}
