//! `ocicri` pod-sandbox lifecycle integration tests (`docs/design/
//! 0233`): spawns the actual built `ocicri` binary as a real server
//! over a real Unix socket and drives the full CRI pod-sandbox state
//! machine — `RunPodSandbox`/`StopPodSandbox`/`RemovePodSandbox`/
//! `PodSandboxStatus`/`ListPodSandbox` — via the exact same shared,
//! generated `tonic` client the server itself uses, including a real
//! server kill/restart to prove records genuinely persist on disk.

use std::collections::HashMap;
use std::path::Path;
use std::process::{Child, Command};
use std::time::Duration;

use oci_cri_types::runtime_service_client::RuntimeServiceClient;
use oci_cri_types::{
    LinuxPodSandboxConfig, LinuxSandboxSecurityContext, ListPodSandboxRequest, NamespaceMode,
    NamespaceOption, PodSandboxConfig, PodSandboxFilter, PodSandboxMetadata, PodSandboxState,
    PodSandboxStateValue, PodSandboxStatusRequest, RemovePodSandboxRequest, RunPodSandboxRequest,
    StopPodSandboxRequest,
};
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

/// A minimal, valid sandbox config (real metadata, one caller-chosen
/// label) — the same shape a real kubelet's own `RunPodSandbox`
/// request has, minus everything optional.
fn sandbox_config(name: &str, uid: &str, attempt: u32) -> PodSandboxConfig {
    PodSandboxConfig {
        metadata: Some(PodSandboxMetadata {
            name: name.to_string(),
            uid: uid.to_string(),
            namespace: "default".to_string(),
            attempt,
        }),
        labels: HashMap::from([("app".to_string(), name.to_string())]),
        annotations: HashMap::from([("test/annotation".to_string(), "kept".to_string())]),
        ..Default::default()
    }
}

fn run_request(config: PodSandboxConfig) -> RunPodSandboxRequest {
    RunPodSandboxRequest {
        config: Some(config),
        runtime_handler: String::new(),
    }
}

/// The full lifecycle, over a real socket: run -> list (one `READY`)
/// -> status -> stop -> status (`NOTREADY`) -> stop again
/// (idempotent) -> remove -> list (empty) -> remove again
/// (idempotent) -> status (a real gRPC `NotFound`).
#[tokio::test]
async fn pod_sandbox_full_lifecycle_over_a_real_unix_socket() {
    let storage_dir = tempfile::tempdir().unwrap();
    let socket_dir = tempfile::tempdir().unwrap();
    let socket_path = socket_dir.path().join("ocicri.sock");
    let _server = spawn_server(storage_dir.path(), &socket_path);
    wait_for_socket(&socket_path);
    let mut client = connect(socket_path).await;

    // Run.
    let sandbox_id = client
        .run_pod_sandbox(run_request(sandbox_config("web", "uid-1", 0)))
        .await
        .expect("RunPodSandbox failed")
        .into_inner()
        .pod_sandbox_id;
    assert_eq!(
        sandbox_id.len(),
        64,
        "a real cri-o-shaped 64-hex sandbox ID, got {sandbox_id:?}"
    );

    // List: exactly one READY sandbox, with the kubelet-default
    // labels populated alongside the caller's own.
    let items = client
        .list_pod_sandbox(ListPodSandboxRequest { filter: None })
        .await
        .expect("ListPodSandbox failed")
        .into_inner()
        .items;
    assert_eq!(items.len(), 1, "{items:?}");
    let listed = &items[0];
    assert_eq!(listed.id, sandbox_id);
    assert_eq!(listed.state, PodSandboxState::SandboxReady as i32);
    assert!(listed.created_at > 0, "created_at must be > 0 (the proto)");
    assert_eq!(listed.labels.get("app"), Some(&"web".to_string()));
    assert_eq!(
        listed.labels.get("io.kubernetes.pod.name"),
        Some(&"web".to_string()),
        "kubelet-default labels should be populated when missing"
    );
    assert_eq!(
        listed.labels.get("io.kubernetes.pod.uid"),
        Some(&"uid-1".to_string())
    );

    // Status: same record, annotations echoed verbatim, an empty (but
    // present) network status, verbose info only when asked for.
    let response = client
        .pod_sandbox_status(PodSandboxStatusRequest {
            pod_sandbox_id: sandbox_id.clone(),
            verbose: false,
        })
        .await
        .expect("PodSandboxStatus failed")
        .into_inner();
    let status = response.status.expect("status should be present");
    assert_eq!(status.id, sandbox_id);
    assert_eq!(status.state, PodSandboxState::SandboxReady as i32);
    assert_eq!(
        status.annotations.get("test/annotation"),
        Some(&"kept".to_string()),
        "annotations MUST be echoed verbatim (the proto)"
    );
    let metadata = status.metadata.expect("metadata should be echoed back");
    assert_eq!(metadata.name, "web");
    assert_eq!(metadata.namespace, "default");
    assert_eq!(metadata.uid, "uid-1");
    assert!(status.network.is_some(), "network status always present");
    assert!(response.info.is_empty(), "info only when verbose");

    let verbose = client
        .pod_sandbox_status(PodSandboxStatusRequest {
            pod_sandbox_id: sandbox_id.clone(),
            verbose: true,
        })
        .await
        .expect("verbose PodSandboxStatus failed")
        .into_inner();
    let info = verbose.info.get("info").expect("verbose info under 'info'");
    let parsed: serde_json::Value = serde_json::from_str(info).expect("info should be real JSON");
    assert_eq!(parsed["id"], serde_json::json!(sandbox_id));

    // A short, unambiguous ID prefix resolves too (real cri-o's own
    // truncindex behavior).
    let by_prefix = client
        .pod_sandbox_status(PodSandboxStatusRequest {
            pod_sandbox_id: sandbox_id[..13].to_string(),
            verbose: false,
        })
        .await
        .expect("PodSandboxStatus by prefix failed")
        .into_inner();
    assert_eq!(by_prefix.status.unwrap().id, sandbox_id);

    // Stop: READY -> NOTREADY, idempotently.
    client
        .stop_pod_sandbox(StopPodSandboxRequest {
            pod_sandbox_id: sandbox_id.clone(),
        })
        .await
        .expect("StopPodSandbox failed");
    let status = client
        .pod_sandbox_status(PodSandboxStatusRequest {
            pod_sandbox_id: sandbox_id.clone(),
            verbose: false,
        })
        .await
        .expect("PodSandboxStatus after stop failed")
        .into_inner()
        .status
        .unwrap();
    assert_eq!(status.state, PodSandboxState::SandboxNotready as i32);
    client
        .stop_pod_sandbox(StopPodSandboxRequest {
            pod_sandbox_id: sandbox_id.clone(),
        })
        .await
        .expect("a second StopPodSandbox should be a real, idempotent success");

    // Remove: gone from list, idempotent on repeat, NotFound on
    // status.
    client
        .remove_pod_sandbox(RemovePodSandboxRequest {
            pod_sandbox_id: sandbox_id.clone(),
        })
        .await
        .expect("RemovePodSandbox failed");
    let items = client
        .list_pod_sandbox(ListPodSandboxRequest { filter: None })
        .await
        .expect("ListPodSandbox after remove failed")
        .into_inner()
        .items;
    assert!(items.is_empty(), "{items:?}");
    client
        .remove_pod_sandbox(RemovePodSandboxRequest {
            pod_sandbox_id: sandbox_id.clone(),
        })
        .await
        .expect("a second RemovePodSandbox should be a real, idempotent success");
    let status = client
        .pod_sandbox_status(PodSandboxStatusRequest {
            pod_sandbox_id: sandbox_id,
            verbose: false,
        })
        .await
        .expect_err("status of a removed sandbox should be an error");
    assert_eq!(status.code(), tonic::Code::NotFound);
}

/// A duplicate request (same name/namespace/uid/attempt) returns the
/// existing sandbox's ID as a success — real cri-o's own
/// `reservePodNameOrGetExisting` duplicate-request branch, which real
/// kubelet retries after a lost response depend on. A different
/// attempt is a genuinely different sandbox.
#[tokio::test]
async fn run_pod_sandbox_with_duplicate_metadata_returns_the_existing_id() {
    let storage_dir = tempfile::tempdir().unwrap();
    let socket_dir = tempfile::tempdir().unwrap();
    let socket_path = socket_dir.path().join("ocicri.sock");
    let _server = spawn_server(storage_dir.path(), &socket_path);
    wait_for_socket(&socket_path);
    let mut client = connect(socket_path).await;

    let first = client
        .run_pod_sandbox(run_request(sandbox_config("dup", "uid-dup", 0)))
        .await
        .expect("first RunPodSandbox failed")
        .into_inner()
        .pod_sandbox_id;
    let second = client
        .run_pod_sandbox(run_request(sandbox_config("dup", "uid-dup", 0)))
        .await
        .expect("duplicate RunPodSandbox should succeed")
        .into_inner()
        .pod_sandbox_id;
    assert_eq!(first, second, "a duplicate request returns the same ID");

    let third = client
        .run_pod_sandbox(run_request(sandbox_config("dup", "uid-dup", 1)))
        .await
        .expect("RunPodSandbox with a new attempt failed")
        .into_inner()
        .pod_sandbox_id;
    assert_ne!(first, third, "a new attempt is a genuinely new sandbox");
}

/// Validation rejections, matching real cri-o's own
/// `SetConfig`/`GenerateNameAndID` checks; unknown-ID stop/remove are
/// silent successes ("the CRI interface ... expects to not error out
/// in not found cases").
#[tokio::test]
async fn run_pod_sandbox_validation_and_not_found_semantics() {
    let storage_dir = tempfile::tempdir().unwrap();
    let socket_dir = tempfile::tempdir().unwrap();
    let socket_path = socket_dir.path().join("ocicri.sock");
    let _server = spawn_server(storage_dir.path(), &socket_path);
    wait_for_socket(&socket_path);
    let mut client = connect(socket_path).await;

    // No config at all.
    let status = client
        .run_pod_sandbox(RunPodSandboxRequest {
            config: None,
            runtime_handler: String::new(),
        })
        .await
        .expect_err("no config should be rejected");
    assert_eq!(status.code(), tonic::Code::InvalidArgument);

    // No metadata.
    let status = client
        .run_pod_sandbox(run_request(PodSandboxConfig::default()))
        .await
        .expect_err("no metadata should be rejected");
    assert_eq!(status.code(), tonic::Code::InvalidArgument);

    // Empty uid.
    let status = client
        .run_pod_sandbox(run_request(sandbox_config("web", "", 0)))
        .await
        .expect_err("an empty uid should be rejected");
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
    assert!(status.message().contains("uid"), "{status:?}");

    // A non-empty runtime handler is unknown by definition here.
    let status = client
        .run_pod_sandbox(RunPodSandboxRequest {
            config: Some(sandbox_config("web", "uid-h", 0)),
            runtime_handler: "kata".to_string(),
        })
        .await
        .expect_err("an unknown runtime handler should be rejected");
    assert_eq!(status.code(), tonic::Code::InvalidArgument);

    // Unknown-ID stop/remove: real, silent successes.
    client
        .stop_pod_sandbox(StopPodSandboxRequest {
            pod_sandbox_id: "deadbeef".repeat(8),
        })
        .await
        .expect("stopping an unknown sandbox should silently succeed");
    client
        .remove_pod_sandbox(RemovePodSandboxRequest {
            pod_sandbox_id: "deadbeef".repeat(8),
        })
        .await
        .expect("removing an unknown sandbox should silently succeed");
}

/// `ListPodSandbox` filters, ANDed like real cri-o's own
/// `filterSandboxList`: by state, by label selector, by (prefix) ID —
/// and an ID matching nothing yields an empty list, never an error.
#[tokio::test]
async fn list_pod_sandbox_filters_by_state_labels_and_id() {
    let storage_dir = tempfile::tempdir().unwrap();
    let socket_dir = tempfile::tempdir().unwrap();
    let socket_path = socket_dir.path().join("ocicri.sock");
    let _server = spawn_server(storage_dir.path(), &socket_path);
    wait_for_socket(&socket_path);
    let mut client = connect(socket_path).await;

    let ready_id = client
        .run_pod_sandbox(run_request(sandbox_config("ready-pod", "uid-r", 0)))
        .await
        .unwrap()
        .into_inner()
        .pod_sandbox_id;
    let stopped_id = client
        .run_pod_sandbox(run_request(sandbox_config("stopped-pod", "uid-s", 0)))
        .await
        .unwrap()
        .into_inner()
        .pod_sandbox_id;
    client
        .stop_pod_sandbox(StopPodSandboxRequest {
            pod_sandbox_id: stopped_id.clone(),
        })
        .await
        .unwrap();

    // By state.
    let ready_only = client
        .list_pod_sandbox(ListPodSandboxRequest {
            filter: Some(PodSandboxFilter {
                state: Some(PodSandboxStateValue {
                    state: PodSandboxState::SandboxReady as i32,
                }),
                ..Default::default()
            }),
        })
        .await
        .unwrap()
        .into_inner()
        .items;
    assert_eq!(ready_only.len(), 1, "{ready_only:?}");
    assert_eq!(ready_only[0].id, ready_id);

    // By label selector.
    let by_label = client
        .list_pod_sandbox(ListPodSandboxRequest {
            filter: Some(PodSandboxFilter {
                label_selector: HashMap::from([("app".to_string(), "stopped-pod".to_string())]),
                ..Default::default()
            }),
        })
        .await
        .unwrap()
        .into_inner()
        .items;
    assert_eq!(by_label.len(), 1, "{by_label:?}");
    assert_eq!(by_label[0].id, stopped_id);

    // By ID prefix, ANDed with a state that doesn't match: empty.
    let none = client
        .list_pod_sandbox(ListPodSandboxRequest {
            filter: Some(PodSandboxFilter {
                id: ready_id[..13].to_string(),
                state: Some(PodSandboxStateValue {
                    state: PodSandboxState::SandboxNotready as i32,
                }),
                ..Default::default()
            }),
        })
        .await
        .unwrap()
        .into_inner()
        .items;
    assert!(none.is_empty(), "{none:?}");

    // An ID matching nothing: an empty list, never an error.
    let unknown = client
        .list_pod_sandbox(ListPodSandboxRequest {
            filter: Some(PodSandboxFilter {
                id: "deadbeef".repeat(8),
                ..Default::default()
            }),
        })
        .await
        .expect("an unknown-ID filter should never be an error")
        .into_inner()
        .items;
    assert!(unknown.is_empty(), "{unknown:?}");
}

/// Sandbox records survive a real server kill/restart against the
/// same storage root — matching real cri-o restoring its own sandbox
/// state from `containers/storage` rather than starting amnesiac.
#[tokio::test]
async fn pod_sandbox_records_survive_a_real_server_restart() {
    let storage_dir = tempfile::tempdir().unwrap();
    let socket_dir = tempfile::tempdir().unwrap();
    let socket_path = socket_dir.path().join("ocicri.sock");

    let sandbox_id = {
        let _server = spawn_server(storage_dir.path(), &socket_path);
        wait_for_socket(&socket_path);
        let mut client = connect(socket_path.clone()).await;
        client
            .run_pod_sandbox(run_request(sandbox_config("survivor", "uid-p", 0)))
            .await
            .expect("RunPodSandbox failed")
            .into_inner()
            .pod_sandbox_id
        // Server killed here (Drop).
    };

    // A fresh server against the same storage root still knows the
    // sandbox.
    std::fs::remove_file(&socket_path).ok();
    let _server = spawn_server(storage_dir.path(), &socket_path);
    wait_for_socket(&socket_path);
    let mut client = connect(socket_path).await;
    let status = client
        .pod_sandbox_status(PodSandboxStatusRequest {
            pod_sandbox_id: sandbox_id.clone(),
            verbose: false,
        })
        .await
        .expect("a restarted server should still know the sandbox")
        .into_inner()
        .status
        .unwrap();
    assert_eq!(status.id, sandbox_id);
    assert_eq!(status.state, PodSandboxState::SandboxReady as i32);
}

/// `PodSandboxStatus` echoes the namespace options the request itself
/// declared (stored verbatim at creation) — matching real cri-o,
/// whose own status echoes the requested config, not a live probe.
#[tokio::test]
async fn pod_sandbox_status_echoes_the_requested_namespace_options() {
    let storage_dir = tempfile::tempdir().unwrap();
    let socket_dir = tempfile::tempdir().unwrap();
    let socket_path = socket_dir.path().join("ocicri.sock");
    let _server = spawn_server(storage_dir.path(), &socket_path);
    wait_for_socket(&socket_path);
    let mut client = connect(socket_path).await;

    let mut config = sandbox_config("ns-pod", "uid-ns", 0);
    config.linux = Some(LinuxPodSandboxConfig {
        security_context: Some(LinuxSandboxSecurityContext {
            namespace_options: Some(NamespaceOption {
                network: NamespaceMode::Node as i32,
                pid: NamespaceMode::Container as i32,
                ipc: NamespaceMode::Pod as i32,
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    });

    let sandbox_id = client
        .run_pod_sandbox(run_request(config))
        .await
        .expect("RunPodSandbox failed")
        .into_inner()
        .pod_sandbox_id;
    let status = client
        .pod_sandbox_status(PodSandboxStatusRequest {
            pod_sandbox_id: sandbox_id,
            verbose: false,
        })
        .await
        .expect("PodSandboxStatus failed")
        .into_inner()
        .status
        .unwrap();

    let options = status
        .linux
        .expect("linux status should be present when namespace options were declared")
        .namespaces
        .expect("namespaces should be present")
        .options
        .expect("options should be present");
    assert_eq!(options.network, NamespaceMode::Node as i32);
    assert_eq!(options.pid, NamespaceMode::Container as i32);
    assert_eq!(options.ipc, NamespaceMode::Pod as i32);
}
