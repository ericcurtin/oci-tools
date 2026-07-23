//! `ocicri` container-lifecycle integration tests (`docs/design/
//! 0236`, the record-keeping first slice: `CreateContainer`/
//! `ContainerStatus`/`ListContainers`/`RemoveContainer`): spawns the
//! actual built `ocicri` binary as a real server over a real Unix
//! socket, pointed at a real, seeded `oci_store::Store` (the same
//! fixture `ocicri_image_service.rs` already uses), and drives the
//! CRI container state machine via the exact same shared, generated
//! `tonic` client the server itself uses.

use std::collections::HashMap;
use std::path::Path;
use std::process::{Child, Command};
use std::time::Duration;

use oci_cri_types::runtime_service_client::RuntimeServiceClient;
use oci_cri_types::{
    ContainerConfig as CriContainerConfig, ContainerFilter, ContainerMetadata, ContainerState,
    ContainerStateValue, ContainerStatusRequest, CreateContainerRequest, ImageSpec,
    ListContainersRequest, PodSandboxConfig, PodSandboxMetadata, RemoveContainerRequest,
    RemovePodSandboxRequest, RunPodSandboxRequest, StopPodSandboxRequest,
};
use oci_spec_types::image::ContainerConfig;
use oci_store::Store;
use oci_tools_tests::{bin_path, busybox_path, seed_image};

const IMAGE: &str = "docker.io/ocicri-test/container-base:latest";

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

fn pod_config(name: &str, uid: &str) -> PodSandboxConfig {
    PodSandboxConfig {
        metadata: Some(PodSandboxMetadata {
            name: name.to_string(),
            uid: uid.to_string(),
            namespace: "default".to_string(),
            attempt: 0,
        }),
        ..Default::default()
    }
}

fn container_config(name: &str, attempt: u32) -> CriContainerConfig {
    CriContainerConfig {
        metadata: Some(ContainerMetadata {
            name: name.to_string(),
            attempt,
        }),
        image: Some(ImageSpec {
            image: IMAGE.to_string(),
            ..Default::default()
        }),
        labels: HashMap::from([("app".to_string(), name.to_string())]),
        annotations: HashMap::from([("test/annotation".to_string(), "kept".to_string())]),
        ..Default::default()
    }
}

/// Spawns a server against a seeded store and creates one READY
/// sandbox, returning everything a container test needs. Returns
/// `None` (skip) when busybox isn't available on this host.
async fn setup() -> Option<(
    tempfile::TempDir,
    tempfile::TempDir,
    Server,
    RuntimeServiceClient<tonic::transport::Channel>,
    String,
    PodSandboxConfig,
)> {
    let busybox = busybox_path()?;
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(&store, IMAGE, &busybox, &["sh"], ContainerConfig::default());

    let socket_dir = tempfile::tempdir().unwrap();
    let socket_path = socket_dir.path().join("ocicri.sock");
    let server = spawn_server(storage_dir.path(), &socket_path);
    wait_for_socket(&socket_path);
    let mut client = connect(socket_path).await;

    let sandbox_config = pod_config("web", "uid-1");
    let sandbox_id = client
        .run_pod_sandbox(RunPodSandboxRequest {
            config: Some(sandbox_config.clone()),
            runtime_handler: String::new(),
        })
        .await
        .expect("RunPodSandbox failed")
        .into_inner()
        .pod_sandbox_id;

    Some((
        storage_dir,
        socket_dir,
        server,
        client,
        sandbox_id,
        sandbox_config,
    ))
}

/// The full created-state lifecycle over a real socket: create ->
/// list (one CREATED) -> status -> remove -> list (empty) -> remove
/// again (idempotent) -> status (NotFound). Duplicate create returns
/// the same ID; a new attempt is a new container.
#[tokio::test]
async fn container_create_status_list_remove_lifecycle() {
    let Some((_storage, _socket, _server, mut client, sandbox_id, sandbox_config)) = setup().await
    else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };

    let container_id = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id.clone(),
            config: Some(container_config("app", 0)),
            sandbox_config: Some(sandbox_config.clone()),
        })
        .await
        .expect("CreateContainer failed")
        .into_inner()
        .container_id;
    assert_eq!(container_id.len(), 64, "{container_id:?}");

    // Duplicate request: same ID back, matching real cri-o's own
    // duplicate-request branch.
    let duplicate = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id.clone(),
            config: Some(container_config("app", 0)),
            sandbox_config: Some(sandbox_config.clone()),
        })
        .await
        .expect("duplicate CreateContainer should succeed")
        .into_inner()
        .container_id;
    assert_eq!(duplicate, container_id);

    // A new attempt is a genuinely new container.
    let second = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id.clone(),
            config: Some(container_config("app", 1)),
            sandbox_config: Some(sandbox_config.clone()),
        })
        .await
        .expect("CreateContainer with a new attempt failed")
        .into_inner()
        .container_id;
    assert_ne!(second, container_id);

    // List: both, CREATED, real image/sandbox linkage.
    let containers = client
        .list_containers(ListContainersRequest { filter: None })
        .await
        .expect("ListContainers failed")
        .into_inner()
        .containers;
    assert_eq!(containers.len(), 2, "{containers:?}");
    let listed = containers
        .iter()
        .find(|c| c.id == container_id)
        .expect("the first container should be listed");
    assert_eq!(listed.state, ContainerState::ContainerCreated as i32);
    assert_eq!(listed.pod_sandbox_id, sandbox_id);
    assert_eq!(listed.image.as_ref().unwrap().image, IMAGE);
    assert!(listed.image_ref.starts_with("sha256:"), "{listed:?}");
    assert!(listed.created_at > 0);

    // Status: metadata/labels/annotations echoed, verbose info only
    // when asked, prefix resolution works.
    let response = client
        .container_status(ContainerStatusRequest {
            container_id: container_id[..13].to_string(),
            verbose: false,
        })
        .await
        .expect("ContainerStatus by prefix failed")
        .into_inner();
    let status = response.status.expect("status should be present");
    assert_eq!(status.id, container_id);
    assert_eq!(status.state, ContainerState::ContainerCreated as i32);
    assert_eq!(status.metadata.as_ref().unwrap().name, "app");
    assert_eq!(
        status.annotations.get("test/annotation"),
        Some(&"kept".to_string())
    );
    assert_eq!(status.started_at, 0, "a CREATED container never started");
    assert!(response.info.is_empty(), "info only when verbose");

    let verbose = client
        .container_status(ContainerStatusRequest {
            container_id: container_id.clone(),
            verbose: true,
        })
        .await
        .expect("verbose ContainerStatus failed")
        .into_inner();
    let info = verbose.info.get("info").expect("verbose info under 'info'");
    let parsed: serde_json::Value = serde_json::from_str(info).expect("info should be real JSON");
    assert_eq!(parsed["id"], serde_json::json!(container_id));

    // Remove: gone, idempotent, NotFound on status afterward.
    client
        .remove_container(RemoveContainerRequest {
            container_id: container_id.clone(),
        })
        .await
        .expect("RemoveContainer failed");
    client
        .remove_container(RemoveContainerRequest {
            container_id: container_id.clone(),
        })
        .await
        .expect("a second RemoveContainer should be a real, idempotent success");
    let not_found = client
        .container_status(ContainerStatusRequest {
            container_id,
            verbose: false,
        })
        .await
        .expect_err("status of a removed container should be an error");
    assert_eq!(not_found.code(), tonic::Code::NotFound);

    let remaining = client
        .list_containers(ListContainersRequest { filter: None })
        .await
        .unwrap()
        .into_inner()
        .containers;
    assert_eq!(remaining.len(), 1, "{remaining:?}");
    assert_eq!(remaining[0].id, second);
}

/// Validation and precondition rejections, each checked against real
/// cri-o's own rules: unknown sandbox, stopped sandbox, missing
/// image (not pulled), unknown-ID remove as silent success.
#[tokio::test]
async fn container_create_validation_and_preconditions() {
    let Some((_storage, _socket, _server, mut client, sandbox_id, sandbox_config)) = setup().await
    else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };

    // Unknown sandbox.
    let status = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: "deadbeef".repeat(8),
            config: Some(container_config("app", 0)),
            sandbox_config: Some(sandbox_config.clone()),
        })
        .await
        .expect_err("an unknown sandbox should be rejected");
    assert_eq!(status.code(), tonic::Code::NotFound);
    assert!(
        status.message().contains("specified sandbox not found"),
        "{status:?}"
    );

    // An image that was never pulled.
    let mut config = container_config("app", 0);
    config.image = Some(ImageSpec {
        image: "docker.io/ocicri-test/never-pulled:latest".to_string(),
        ..Default::default()
    });
    let status = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id.clone(),
            config: Some(config),
            sandbox_config: Some(sandbox_config.clone()),
        })
        .await
        .expect_err("an unpulled image should be rejected");
    assert_eq!(status.code(), tonic::Code::NotFound);
    assert!(
        status.message().contains("not present locally"),
        "{status:?}"
    );

    // A stopped sandbox refuses new containers (real cri-o's own
    // "CreateContainer failed as the sandbox was stopped").
    client
        .stop_pod_sandbox(StopPodSandboxRequest {
            pod_sandbox_id: sandbox_id.clone(),
        })
        .await
        .unwrap();
    let status = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id.clone(),
            config: Some(container_config("app", 0)),
            sandbox_config: Some(sandbox_config.clone()),
        })
        .await
        .expect_err("a stopped sandbox should refuse new containers");
    assert_eq!(status.code(), tonic::Code::FailedPrecondition);
    assert!(
        status.message().contains("sandbox was stopped"),
        "{status:?}"
    );

    // Unknown-ID remove: a real, silent success.
    client
        .remove_container(RemoveContainerRequest {
            container_id: "deadbeef".repeat(8),
        })
        .await
        .expect("removing an unknown container should silently succeed");
}

/// `ListContainers` filters, ANDed like real cri-o's own
/// `filterContainerList`: by sandbox, by state, by label selector,
/// by id+sandbox combination; a filter matching nothing is an empty
/// list, never an error.
#[tokio::test]
async fn list_containers_filters_by_sandbox_state_and_labels() {
    let Some((_storage, _socket, _server, mut client, sandbox_id, sandbox_config)) = setup().await
    else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };

    // A second sandbox with its own container.
    let other_config = pod_config("other", "uid-2");
    let other_sandbox = client
        .run_pod_sandbox(RunPodSandboxRequest {
            config: Some(other_config.clone()),
            runtime_handler: String::new(),
        })
        .await
        .unwrap()
        .into_inner()
        .pod_sandbox_id;

    let in_first = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id.clone(),
            config: Some(container_config("app-a", 0)),
            sandbox_config: Some(sandbox_config.clone()),
        })
        .await
        .unwrap()
        .into_inner()
        .container_id;
    let in_other = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: other_sandbox.clone(),
            config: Some(container_config("app-b", 0)),
            sandbox_config: Some(other_config.clone()),
        })
        .await
        .unwrap()
        .into_inner()
        .container_id;

    // By sandbox (prefix).
    let by_sandbox = client
        .list_containers(ListContainersRequest {
            filter: Some(ContainerFilter {
                pod_sandbox_id: other_sandbox[..13].to_string(),
                ..Default::default()
            }),
        })
        .await
        .unwrap()
        .into_inner()
        .containers;
    assert_eq!(by_sandbox.len(), 1, "{by_sandbox:?}");
    assert_eq!(by_sandbox[0].id, in_other);

    // By state: everything this slice makes is CREATED, so a RUNNING
    // filter is a real empty list.
    let running_only = client
        .list_containers(ListContainersRequest {
            filter: Some(ContainerFilter {
                state: Some(ContainerStateValue {
                    state: ContainerState::ContainerRunning as i32,
                }),
                ..Default::default()
            }),
        })
        .await
        .unwrap()
        .into_inner()
        .containers;
    assert!(running_only.is_empty(), "{running_only:?}");

    // By label selector.
    let by_label = client
        .list_containers(ListContainersRequest {
            filter: Some(ContainerFilter {
                label_selector: HashMap::from([("app".to_string(), "app-a".to_string())]),
                ..Default::default()
            }),
        })
        .await
        .unwrap()
        .into_inner()
        .containers;
    assert_eq!(by_label.len(), 1, "{by_label:?}");
    assert_eq!(by_label[0].id, in_first);

    // id + a sandbox it doesn't belong to: empty (real cri-o's own
    // HasPrefix cross-check).
    let mismatched = client
        .list_containers(ListContainersRequest {
            filter: Some(ContainerFilter {
                id: in_first.clone(),
                pod_sandbox_id: other_sandbox.clone(),
                ..Default::default()
            }),
        })
        .await
        .unwrap()
        .into_inner()
        .containers;
    assert!(mismatched.is_empty(), "{mismatched:?}");

    // An unknown-ID filter: empty, never an error.
    let unknown = client
        .list_containers(ListContainersRequest {
            filter: Some(ContainerFilter {
                id: "deadbeef".repeat(8),
                ..Default::default()
            }),
        })
        .await
        .expect("an unknown-ID filter should never be an error")
        .into_inner()
        .containers;
    assert!(unknown.is_empty(), "{unknown:?}");
}

/// `RemovePodSandbox` forcibly removes the sandbox's own containers
/// too (the proto's own contract, real cri-o's own
/// `removePodSandbox` loop) — and container records survive a real
/// server restart just like sandbox records do.
#[tokio::test]
async fn remove_pod_sandbox_cascades_and_records_survive_restart() {
    let Some((storage, socket, server, mut client, sandbox_id, sandbox_config)) = setup().await
    else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };

    let container_id = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id.clone(),
            config: Some(container_config("app", 0)),
            sandbox_config: Some(sandbox_config.clone()),
        })
        .await
        .unwrap()
        .into_inner()
        .container_id;

    // Kill and restart the server against the same storage root: the
    // container record is still there.
    drop(server);
    let socket_path = socket.path().join("ocicri.sock");
    std::fs::remove_file(&socket_path).ok();
    let _server = spawn_server(storage.path(), &socket_path);
    wait_for_socket(&socket_path);
    let mut client2 = connect(socket_path).await;
    let status = client2
        .container_status(ContainerStatusRequest {
            container_id: container_id.clone(),
            verbose: false,
        })
        .await
        .expect("a restarted server should still know the container")
        .into_inner()
        .status
        .unwrap();
    assert_eq!(status.state, ContainerState::ContainerCreated as i32);

    // Removing the sandbox removes its containers too.
    client2
        .remove_pod_sandbox(RemovePodSandboxRequest {
            pod_sandbox_id: sandbox_id,
        })
        .await
        .expect("RemovePodSandbox failed");
    let not_found = client2
        .container_status(ContainerStatusRequest {
            container_id,
            verbose: false,
        })
        .await
        .expect_err("the sandbox's container should be gone too");
    assert_eq!(not_found.code(), tonic::Code::NotFound);
    let remaining = client2
        .list_containers(ListContainersRequest { filter: None })
        .await
        .unwrap()
        .into_inner()
        .containers;
    assert!(remaining.is_empty(), "{remaining:?}");
}
