//! `ocicri` integration test: spawns the actual built `ocicri` binary
//! as a real server listening on a real Unix domain socket, connects a
//! real, generated `tonic` gRPC client (from the exact same shared
//! `oci_cri_types` crate the server itself uses — never a hand-rolled
//! protocol stand-in that could silently drift from the real wire
//! format), and calls the one real RPC this first slice answers,
//! `RuntimeService.Version`.

use std::path::Path;
use std::process::{Child, Command};
use std::time::Duration;

use oci_cri_types::VersionRequest;
use oci_cri_types::runtime_service_client::RuntimeServiceClient;
use oci_tools_tests::bin_path;

/// A running `ocicri` server, killed on drop so a failing test (or an
/// early return) never leaves a stray server process behind.
struct Server {
    child: Child,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_server(socket_path: &Path) -> Server {
    let child = Command::new(bin_path("ocicri"))
        .env_remove("OCI_TOOLS_LOG")
        .args(["--listen", socket_path.to_str().unwrap()])
        .spawn()
        .expect("failed to spawn ocicri");
    Server { child }
}

/// Waits for `ocicri`'s own real socket file to appear, matching the
/// real, unavoidable "server takes a moment to bind after being
/// spawned" race any real socket-based integration test has to
/// tolerate.
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

async fn connect(
    socket_path: std::path::PathBuf,
) -> RuntimeServiceClient<tonic::transport::Channel> {
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
    RuntimeServiceClient::new(channel)
}

#[tokio::test]
async fn version_reports_real_honest_values_over_a_real_unix_socket() {
    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("ocicri.sock");
    let _server = spawn_server(&socket_path);
    wait_for_socket(&socket_path);

    let mut client = connect(socket_path).await;
    let response = client
        .version(VersionRequest {
            version: "0.1.0".to_string(),
        })
        .await
        .expect("Version RPC failed")
        .into_inner();

    assert_eq!(response.version, "0.1.0");
    assert_eq!(response.runtime_name, "ocicri");
    assert_eq!(response.runtime_api_version, "v1");
    assert!(
        !response.runtime_version.is_empty(),
        "runtime_version should be a real, non-empty build version string"
    );
}

/// Every other real RPC returns a real, honest `Unimplemented` gRPC
/// status naming itself, rather than the connection silently hanging
/// or erroring in some other, less informative way -- confirmed over
/// the exact same real wire protocol a real `crictl`/kubelet would
/// use, not just the in-process unit test already covering this in
/// `bin/ocicri/src/runtime_service.rs`.
#[tokio::test]
async fn an_unimplemented_rpc_is_a_real_honest_status_over_the_wire() {
    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("ocicri.sock");
    let _server = spawn_server(&socket_path);
    wait_for_socket(&socket_path);

    let mut client = connect(socket_path).await;
    let status = client
        .list_pod_sandbox(oci_cri_types::ListPodSandboxRequest { filter: None })
        .await
        .expect_err("ListPodSandbox should be a real, honest error, not a success");

    assert_eq!(status.code(), tonic::Code::Unimplemented);
    assert!(status.message().contains("ListPodSandbox"), "{status:?}");
}
