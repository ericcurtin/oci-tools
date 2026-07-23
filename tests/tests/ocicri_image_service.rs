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
use oci_cri_types::{
    ImageFilter, ImageFsInfoRequest, ImageSpec, ImageStatusRequest, ListImagesRequest,
    RemoveImageRequest, StreamImagesRequest,
};
use oci_spec_types::image::ContainerConfig;
use oci_store::{ImageRecord, Store};
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

#[tokio::test]
async fn remove_image_removes_a_real_seeded_image() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ocicri-test/remove-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let socket_dir = tempfile::tempdir().unwrap();
    let socket_path = socket_dir.path().join("ocicri.sock");
    let _server = spawn_server(storage_dir.path(), &socket_path);
    wait_for_socket(&socket_path);

    let mut client = connect(socket_path).await;
    client
        .remove_image(RemoveImageRequest {
            image: Some(ImageSpec {
                image: "ocicri-test/remove-base:latest".to_string(),
                ..Default::default()
            }),
        })
        .await
        .expect("RemoveImage failed");

    let list = client
        .list_images(ListImagesRequest { filter: None })
        .await
        .unwrap()
        .into_inner();
    assert!(list.images.is_empty(), "{:?}", list.images);
}

/// The real proto's own documented contract, genuinely different from
/// `ociman rmi`'s own (no `--force` ambiguity gate at all): removing
/// *any one* tag resolving to an image removes *every* real reference
/// sharing that same manifest digest.
#[tokio::test]
async fn remove_image_by_one_tag_removes_every_sibling_tag_too() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ocicri-test/sibling-a:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );
    let manifest_digest = store
        .resolve_image("docker.io/ocicri-test/sibling-a:latest")
        .unwrap()
        .unwrap()
        .manifest_digest;
    store
        .put_image(&ImageRecord {
            reference: "docker.io/ocicri-test/sibling-b:latest".to_string(),
            manifest_digest,
        })
        .unwrap();

    let socket_dir = tempfile::tempdir().unwrap();
    let socket_path = socket_dir.path().join("ocicri.sock");
    let _server = spawn_server(storage_dir.path(), &socket_path);
    wait_for_socket(&socket_path);

    let mut client = connect(socket_path).await;
    client
        .remove_image(RemoveImageRequest {
            image: Some(ImageSpec {
                image: "ocicri-test/sibling-a:latest".to_string(),
                ..Default::default()
            }),
        })
        .await
        .expect("RemoveImage failed");

    let list = client
        .list_images(ListImagesRequest { filter: None })
        .await
        .unwrap()
        .into_inner();
    assert!(
        list.images.is_empty(),
        "removing one tag should have removed its sibling tag too: {:?}",
        list.images
    );
}

#[tokio::test]
async fn remove_image_of_an_already_removed_image_is_a_real_silent_success() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let socket_dir = tempfile::tempdir().unwrap();
    let socket_path = socket_dir.path().join("ocicri.sock");
    let _server = spawn_server(storage_dir.path(), &socket_path);
    wait_for_socket(&socket_path);

    let mut client = connect(socket_path).await;
    client
        .remove_image(RemoveImageRequest {
            image: Some(ImageSpec {
                image: "ocicri-test/does-not-exist:latest".to_string(),
                ..Default::default()
            }),
        })
        .await
        .expect("RemoveImage of a nonexistent image must be idempotent, not an error");
}

#[tokio::test]
async fn remove_image_with_no_image_specified_is_a_real_invalid_argument_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let socket_dir = tempfile::tempdir().unwrap();
    let socket_path = socket_dir.path().join("ocicri.sock");
    let _server = spawn_server(storage_dir.path(), &socket_path);
    wait_for_socket(&socket_path);

    let mut client = connect(socket_path).await;
    let status = client
        .remove_image(RemoveImageRequest { image: None })
        .await
        .expect_err("no image specified should be a real error");

    assert_eq!(status.code(), tonic::Code::InvalidArgument);
}

/// `ImageFsInfo` on a store holding a real, seeded image reports a
/// real, non-zero `used_bytes`/`inodes_used` for `image_filesystems`
/// (the blob store actually holds real bytes now) — checked as an
/// honest lower bound, not an exact byte count (this project's own
/// hardlink-deduplicated `oci_store::dir_stats` is deliberately more
/// precise than real cri-o's own cruder walk, so pinning an exact
/// number here would just be re-deriving the same arithmetic under
/// test rather than confirming the RPC's own real behavior).
#[tokio::test]
async fn image_fs_info_reports_real_nonzero_usage_once_an_image_is_stored() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(
        &store,
        "ocicri-test/fs-info-base:latest",
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
        .image_fs_info(ImageFsInfoRequest {})
        .await
        .expect("ImageFsInfo should succeed")
        .into_inner();

    assert_eq!(response.image_filesystems.len(), 1);
    let image_fs = &response.image_filesystems[0];
    assert!(image_fs.timestamp > 0);
    assert!(
        image_fs
            .fs_id
            .as_ref()
            .unwrap()
            .mountpoint
            .contains("blobs"),
        "got: {:?}",
        image_fs.fs_id
    );
    assert!(
        image_fs.used_bytes.as_ref().unwrap().value > 0,
        "a real seeded image should occupy real, nonzero bytes in the blob store"
    );
    assert!(image_fs.inodes_used.as_ref().unwrap().value > 0);

    // `container_filesystems` is real too (the rootfs cache), just
    // legitimately empty here -- no container has ever actually run
    // (nothing has extracted this image's own rootfs into the cache
    // yet), which this RPC reports as a real, honest zero rather than
    // an error.
    assert_eq!(response.container_filesystems.len(), 1);
    let container_fs = &response.container_filesystems[0];
    assert!(container_fs.timestamp > 0);
    assert_eq!(container_fs.used_bytes.as_ref().unwrap().value, 0);
    assert_eq!(container_fs.inodes_used.as_ref().unwrap().value, 0);
}

/// `ImageFsInfo` on a completely empty store (nothing pulled, nothing
/// ever run) is a real, honest all-zero report, not an error — both
/// `blobs_dir` (created eagerly by `Store::open`) and `cache_root`
/// (never created until something actually extracts a rootfs) are
/// covered, the latter exercising the "directory doesn't exist yet"
/// path directly.
#[tokio::test]
async fn image_fs_info_on_an_empty_store_is_a_real_zero_not_an_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    Store::open(storage_dir.path()).unwrap();

    let socket_dir = tempfile::tempdir().unwrap();
    let socket_path = socket_dir.path().join("ocicri.sock");
    let _server = spawn_server(storage_dir.path(), &socket_path);
    wait_for_socket(&socket_path);

    let mut client = connect(socket_path).await;
    let response = client
        .image_fs_info(ImageFsInfoRequest {})
        .await
        .expect("ImageFsInfo on an empty store should not be a gRPC error")
        .into_inner();

    for fs in response
        .image_filesystems
        .iter()
        .chain(response.container_filesystems.iter())
    {
        assert_eq!(fs.used_bytes.as_ref().unwrap().value, 0);
        assert_eq!(fs.inodes_used.as_ref().unwrap().value, 0);
        assert!(fs.timestamp > 0);
    }
}

/// `StreamImages` (`CRIListStreaming`, `docs/design/0234`) returns
/// the exact same items `ListImages` reports — in one message here
/// (far fewer than real cri-o's own 3000-item chunk size) — and
/// streams zero messages (EOF immediately) for an empty store,
/// matching real cri-o's own chunking loop simply never iterating.
#[tokio::test]
async fn stream_images_matches_list_and_streams_nothing_when_empty() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();

    let socket_dir = tempfile::tempdir().unwrap();
    let socket_path = socket_dir.path().join("ocicri.sock");
    let _server = spawn_server(storage_dir.path(), &socket_path);
    wait_for_socket(&socket_path);
    let mut client = connect(socket_path).await;

    // Empty store: a real, successful stream with zero messages.
    let mut empty_stream = client
        .stream_images(StreamImagesRequest { filter: None })
        .await
        .expect("StreamImages on an empty store should succeed")
        .into_inner();
    assert!(
        empty_stream
            .message()
            .await
            .expect("stream should end cleanly")
            .is_none(),
        "an empty store should stream zero messages before EOF"
    );

    seed_image(
        &store,
        "ocicri-test/stream-base:latest",
        &busybox,
        &["sh"],
        ContainerConfig::default(),
    );

    let listed = client
        .list_images(ListImagesRequest { filter: None })
        .await
        .unwrap()
        .into_inner()
        .images;
    let mut stream = client
        .stream_images(StreamImagesRequest { filter: None })
        .await
        .expect("StreamImages failed")
        .into_inner();
    let mut streamed = Vec::new();
    while let Some(response) = stream.message().await.expect("stream should end cleanly") {
        streamed.extend(response.images);
    }
    assert_eq!(streamed, listed, "stream and list must report identically");
    assert_eq!(streamed.len(), 1);
    assert_eq!(
        streamed[0].repo_tags,
        vec!["docker.io/ocicri-test/stream-base:latest".to_string()]
    );
}
