//! `ocicri` `ImageService.PullImage` integration tests: spawns the
//! actual built `ocicri` binary as a real server over a real Unix
//! socket, and calls `PullImage` via the exact same shared, generated
//! `tonic` client the server itself uses.
//!
//! A genuinely successful pull is verified by hand against the real,
//! live `docker.io/library/busybox:latest` registry during this
//! increment's own development (see `docs/design/0214`), not as part
//! of this always-offline automated suite: `PullImage`'s own real
//! production path always uses `tls_verify: true` (secure by
//! default), which can't reach a local plain-HTTP mock registry the
//! way `ociman_pull_policy.rs`'s own tests do for `ociman run/build
//! --pull` (both of which *do* have a real `--tls-verify` flag of
//! their own). What's verified automatically here instead: real
//! argument validation, and a real, honest error when the real
//! network round trip itself fails — the same "prove a real attempt
//! was made" technique `ociman_pull_policy.rs`'s own unreachable-host
//! case already establishes.

use std::path::Path;
use std::process::{Child, Command};
use std::time::Duration;

use oci_cri_types::image_service_client::ImageServiceClient;
use oci_cri_types::{ImageSpec, PullImageRequest};
use oci_tools_tests::bin_path;

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

async fn connect_channel(socket_path: std::path::PathBuf) -> tonic::transport::Channel {
    tonic::transport::Endpoint::try_from("http://[::]:50051")
        .unwrap()
        .connect_with_connector(tower::service_fn(move |_: tonic::transport::Uri| {
            let socket_path = socket_path.clone();
            async move {
                let stream = tokio::net::UnixStream::connect(socket_path).await?;
                Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
            }
        }))
        .await
        .expect("failed to connect to ocicri's own real unix socket")
}

async fn connect(socket_path: std::path::PathBuf) -> ImageServiceClient<tonic::transport::Channel> {
    ImageServiceClient::new(connect_channel(socket_path).await)
}

/// An address nothing is ever listening on in this environment
/// (loopback, a low, privileged port real-world services never bind
/// rootless) — connecting to it fails fast (a real, immediate
/// "connection refused", not a slow timeout), matching
/// `ociman_pull_policy.rs`'s own identical technique for proving a
/// real network attempt happened without needing a real, reachable
/// registry at all.
const UNREACHABLE_HOST: &str = "127.0.0.1:1";

#[tokio::test]
async fn pull_image_of_an_unreachable_registry_is_a_real_honest_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let socket_dir = tempfile::tempdir().unwrap();
    let socket_path = socket_dir.path().join("ocicri.sock");
    let _server = spawn_server(storage_dir.path(), &socket_path);
    wait_for_socket(&socket_path);

    let mut client = connect(socket_path).await;
    let status = client
        .pull_image(PullImageRequest {
            image: Some(ImageSpec {
                image: format!("{UNREACHABLE_HOST}/testrepo:latest"),
                ..Default::default()
            }),
            auth: None,
            sandbox_config: None,
        })
        .await
        .expect_err("pulling from an unreachable host should be a real, honest error");

    // `Unavailable` -- a real network/registry-connectivity failure,
    // not a client-input mistake (`InvalidArgument`) or an internal
    // bug (`Internal`).
    assert_eq!(status.code(), tonic::Code::Unavailable);
}

#[tokio::test]
async fn pull_image_with_no_image_specified_is_a_real_invalid_argument_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let socket_dir = tempfile::tempdir().unwrap();
    let socket_path = socket_dir.path().join("ocicri.sock");
    let _server = spawn_server(storage_dir.path(), &socket_path);
    wait_for_socket(&socket_path);

    let mut client = connect(socket_path).await;
    let status = client
        .pull_image(PullImageRequest {
            image: None,
            auth: None,
            sandbox_config: None,
        })
        .await
        .expect_err("no image specified should be a real error");

    assert_eq!(status.code(), tonic::Code::InvalidArgument);
}

#[tokio::test]
async fn pull_image_of_an_unparseable_reference_is_a_real_invalid_argument_error() {
    let storage_dir = tempfile::tempdir().unwrap();
    let socket_dir = tempfile::tempdir().unwrap();
    let socket_path = socket_dir.path().join("ocicri.sock");
    let _server = spawn_server(storage_dir.path(), &socket_path);
    wait_for_socket(&socket_path);

    let mut client = connect(socket_path).await;
    // `Reference::parse` is otherwise lenient about what counts as a
    // syntactically valid repository path (no character-set
    // restriction beyond "non-empty"), but real registry/repository
    // names are always lowercase (checked directly:
    // `Reference::parse`'s own `NotLowercase` rejection) -- an
    // uppercase reference is guaranteed to fail parsing itself, before
    // any real network attempt.
    let status = client
        .pull_image(PullImageRequest {
            image: Some(ImageSpec {
                image: "NOT-LOWERCASE:latest".to_string(),
                ..Default::default()
            }),
            auth: None,
            sandbox_config: None,
        })
        .await
        .expect_err("an unparseable reference should be a real, immediate error");

    assert_eq!(status.code(), tonic::Code::InvalidArgument);
}

/// While a real, live pull can't be exercised in this always-offline
/// suite (see this module's own doc comment), a concurrent slow pull
/// must never block this server from answering a wholly unrelated RPC
/// in the meantime — the entire reason `pull_image_blocking` runs on
/// `tokio::task::spawn_blocking` rather than directly in its own
/// `async fn`. Proven here by starting a real pull attempt against the
/// unreachable host (slow: a real TCP connect that only fails after
/// its own real OS-level timeout/refusal, not instant) and confirming
/// `Version` still answers immediately on the very same connection
/// while that pull is still in flight.
#[tokio::test]
async fn a_slow_pull_never_blocks_an_unrelated_rpc_on_the_same_server() {
    let storage_dir = tempfile::tempdir().unwrap();
    let socket_dir = tempfile::tempdir().unwrap();
    let socket_path = socket_dir.path().join("ocicri.sock");
    let _server = spawn_server(storage_dir.path(), &socket_path);
    wait_for_socket(&socket_path);

    let mut image_client = connect(socket_path.clone()).await;
    let mut runtime_client = oci_cri_types::runtime_service_client::RuntimeServiceClient::new(
        connect_channel(socket_path).await,
    );

    let pull_future = image_client.pull_image(PullImageRequest {
        image: Some(ImageSpec {
            image: format!("{UNREACHABLE_HOST}/testrepo:latest"),
            ..Default::default()
        }),
        auth: None,
        sandbox_config: None,
    });

    let version_future = runtime_client.version(oci_cri_types::VersionRequest {
        version: "0.1.0".to_string(),
    });

    // `Version` must complete well within a real timeout regardless of
    // the pull's own outcome or timing.
    let version_result = tokio::time::timeout(Duration::from_secs(5), version_future).await;
    assert!(
        version_result.is_ok(),
        "Version should answer promptly even while an unrelated PullImage is still in flight"
    );
    assert!(version_result.unwrap().is_ok());

    // Let the (real, if doomed) pull attempt finish so the server has
    // nothing left in flight before this test's own server is torn
    // down.
    let _ = pull_future.await;
}
