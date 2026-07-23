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

use oci_cri_types::runtime_service_client::RuntimeServiceClient;
use oci_cri_types::{
    CgroupDriver, ListMetricDescriptorsRequest, RuntimeConfigRequest, StatusRequest,
    UpdateRuntimeConfigRequest, VersionRequest,
};
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

/// `Status` reports a real `RuntimeReady=true`/`NetworkReady=false`
/// pair (never fabricating network readiness this project doesn't
/// actually have — see `docs/design/0228`), a real default runtime
/// handler entry, and stays honestly empty in `info` unless
/// `verbose` is set.
#[tokio::test]
async fn status_reports_real_runtime_and_network_conditions_over_a_real_unix_socket() {
    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("ocicri.sock");
    let _server = spawn_server(&socket_path);
    wait_for_socket(&socket_path);

    let mut client = connect(socket_path).await;
    let response = client
        .status(StatusRequest { verbose: false })
        .await
        .expect("Status RPC failed")
        .into_inner();

    let status = response.status.expect("status should always be present");
    let runtime_ready = status
        .conditions
        .iter()
        .find(|c| c.r#type == "RuntimeReady")
        .expect("RuntimeReady condition should be present");
    assert!(runtime_ready.status);

    let network_ready = status
        .conditions
        .iter()
        .find(|c| c.r#type == "NetworkReady")
        .expect("NetworkReady condition should be present");
    assert!(
        !network_ready.status,
        "ocicri sets up no container networking of its own yet"
    );
    assert!(!network_ready.reason.is_empty());
    assert!(!network_ready.message.is_empty());

    assert_eq!(response.runtime_handlers.len(), 1);
    assert_eq!(response.runtime_handlers[0].name, "");

    assert!(
        response.info.is_empty(),
        "info should stay empty when verbose is false"
    );
}

/// `verbose: true` populates `info` with real, already-known values —
/// never fabricated debug data this project doesn't actually have.
#[tokio::test]
async fn status_verbose_populates_a_real_non_empty_info_map() {
    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("ocicri.sock");
    let _server = spawn_server(&socket_path);
    wait_for_socket(&socket_path);

    let mut client = connect(socket_path).await;
    let response = client
        .status(StatusRequest { verbose: true })
        .await
        .expect("Status RPC failed")
        .into_inner();

    assert!(!response.info.is_empty());
    assert_eq!(response.info.get("runtimeName").unwrap(), "ocicri");
    assert!(!response.info.get("runtimeVersion").unwrap().is_empty());
}

/// `RuntimeConfig` reports the real cgroup driver `ociman run`/
/// `create` actually, unconditionally uses today (systemd) — see
/// `docs/design/0229`.
#[tokio::test]
async fn runtime_config_reports_the_real_systemd_cgroup_driver() {
    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("ocicri.sock");
    let _server = spawn_server(&socket_path);
    wait_for_socket(&socket_path);

    let mut client = connect(socket_path).await;
    let response = client
        .runtime_config(RuntimeConfigRequest {})
        .await
        .expect("RuntimeConfig RPC failed")
        .into_inner();

    let linux = response.linux.expect("linux config should be present");
    assert_eq!(linux.cgroup_driver, CgroupDriver::Systemd as i32);
}

/// `UpdateRuntimeConfig` is a real, unconditional no-op — matching
/// real `cri-o` exactly (it discards the given pod CIDR silently,
/// see `docs/design/0229`) — succeeds regardless of what's in the
/// request.
#[tokio::test]
async fn update_runtime_config_is_a_real_unconditional_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("ocicri.sock");
    let _server = spawn_server(&socket_path);
    wait_for_socket(&socket_path);

    let mut client = connect(socket_path).await;
    client
        .update_runtime_config(UpdateRuntimeConfigRequest {
            runtime_config: Some(oci_cri_types::RuntimeConfig {
                network_config: Some(oci_cri_types::NetworkConfig {
                    pod_cidr: "10.244.0.0/16".to_string(),
                }),
            }),
        })
        .await
        .expect("UpdateRuntimeConfig should always succeed");
}

/// `ListMetricDescriptors` reports a real, honest empty list — `ocicri`
/// has no metrics-collection machinery of its own at all yet, so
/// advertising even real cri-o's own one always-on descriptor would
/// be a real, false claim (see `docs/design/0231`).
#[tokio::test]
async fn list_metric_descriptors_reports_a_real_honest_empty_list() {
    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("ocicri.sock");
    let _server = spawn_server(&socket_path);
    wait_for_socket(&socket_path);

    let mut client = connect(socket_path).await;
    let response = client
        .list_metric_descriptors(ListMetricDescriptorsRequest {})
        .await
        .expect("ListMetricDescriptors RPC failed")
        .into_inner();

    assert!(response.descriptors.is_empty());
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
        .attach(oci_cri_types::AttachRequest {
            container_id: "deadbeef".repeat(8),
            ..Default::default()
        })
        .await
        .expect_err("Attach should be a real, honest error, not a success");

    assert_eq!(status.code(), tonic::Code::Unimplemented);
    assert!(status.message().contains("Attach"), "{status:?}");
}
