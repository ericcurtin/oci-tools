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
        // The seeded fixture image declares no Entrypoint/Cmd of its
        // own, so the CRI config supplies the command -- exactly what
        // a real kubelet does for a pod spec with `command:` set.
        command: vec!["/bin/sh".to_string()],
        labels: HashMap::from([("app".to_string(), name.to_string())]),
        annotations: HashMap::from([("test/annotation".to_string(), "kept".to_string())]),
        ..Default::default()
    }
}

/// The bundle directory `CreateContainer` prepares for one container
/// (`docs/design/0237`) — under the test's own private storage root.
fn bundle_dir(storage_root: &Path, container_id: &str) -> std::path::PathBuf {
    storage_root.join("cri-bundles").join(container_id)
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
    // `sleep`/`true` alongside `sh`: the start/stop lifecycle tests
    // (0238) run real containers from this image.
    seed_image(
        &store,
        IMAGE,
        &busybox,
        &["sh", "sleep", "true"],
        ContainerConfig::default(),
    );

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
/// the same ID; a new attempt is a new container. The created
/// container's own real, launch-ready bundle (0237) exists while the
/// container does and is gone when it is.
#[tokio::test]
async fn container_create_status_list_remove_lifecycle() {
    let Some((storage, _socket, _server, mut client, sandbox_id, sandbox_config)) = setup().await
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

    // A real, launch-ready bundle exists (0237): a dedicated,
    // extracted rootfs plus a generated config.json whose process
    // half reflects the CRI config.
    let bundle = bundle_dir(storage.path(), &container_id);
    assert!(
        bundle.join("rootfs/bin/sh").exists(),
        "the bundle rootfs should be a real extraction of the image"
    );
    let spec: serde_json::Value =
        serde_json::from_slice(&std::fs::read(bundle.join("config.json")).unwrap())
            .expect("config.json should be real JSON");
    assert_eq!(spec["process"]["args"], serde_json::json!(["/bin/sh"]));
    // A writable rootfs: `readonly` is serialized as `false` or
    // omitted entirely (the field is skipped when false), never
    // `true`.
    assert_ne!(spec["root"]["readonly"], serde_json::json!(true));

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
            container_id: container_id.clone(),
            verbose: false,
        })
        .await
        .expect_err("status of a removed container should be an error");
    assert_eq!(not_found.code(), tonic::Code::NotFound);
    assert!(
        !bundle_dir(storage.path(), &container_id).exists(),
        "RemoveContainer should remove the bundle too"
    );

    let remaining = client
        .list_containers(ListContainersRequest { filter: None })
        .await
        .unwrap()
        .into_inner()
        .containers;
    assert_eq!(remaining.len(), 1, "{remaining:?}");
    assert_eq!(remaining[0].id, second);
}

/// The CRI-command/args-versus-image-Entrypoint/Cmd merge (real
/// cri-o's own `SpecSetProcessArgs` rule) lands in the generated
/// bundle spec — checked end to end through a real image whose config
/// declares an Entrypoint, plus the "nothing to run anywhere" error,
/// which must leave no half-created bundle behind.
#[tokio::test]
async fn bundle_spec_merges_image_and_cri_config_and_rejects_no_command() {
    let Some((storage, _socket, _server, mut client, sandbox_id, sandbox_config)) = setup().await
    else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };

    // A second image, this one with a real declared Entrypoint and
    // env, seeded into the same store the running server reads.
    let entrypoint_image = "docker.io/ocicri-test/with-entrypoint:latest";
    let busybox = busybox_path().unwrap();
    let store = Store::open(storage.path()).unwrap();
    seed_image(
        &store,
        entrypoint_image,
        &busybox,
        &["sh"],
        ContainerConfig {
            entrypoint: Some(vec!["/bin/sh".to_string()]),
            env: vec!["FROM_IMAGE=1".to_string()],
            ..Default::default()
        },
    );

    // CRI args only: image entrypoint + given args (image cmd
    // ignored), image env first then the kubelet-supplied env.
    let mut config = container_config("merge", 0);
    config.image = Some(ImageSpec {
        image: entrypoint_image.to_string(),
        ..Default::default()
    });
    config.command = Vec::new();
    config.args = vec!["-c".to_string(), "true".to_string()];
    config.envs = vec![oci_cri_types::KeyValue {
        key: "FROM_KUBE".to_string(),
        value: "2".to_string(),
    }];
    let container_id = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id.clone(),
            config: Some(config),
            sandbox_config: Some(sandbox_config.clone()),
        })
        .await
        .expect("CreateContainer with args-only should succeed")
        .into_inner()
        .container_id;
    let spec: serde_json::Value = serde_json::from_slice(
        &std::fs::read(bundle_dir(storage.path(), &container_id).join("config.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        spec["process"]["args"],
        serde_json::json!(["/bin/sh", "-c", "true"])
    );
    let env: Vec<String> = spec["process"]["env"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(env, vec!["FROM_IMAGE=1", "FROM_KUBE=2"]);

    // Nothing to run anywhere: real cri-o's own "no command
    // specified", and no half-created bundle left behind.
    let bundles_before: Vec<_> = std::fs::read_dir(storage.path().join("cri-bundles"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    let mut config = container_config("nothing", 0);
    config.command = Vec::new(); // fixture image has no Entrypoint/Cmd
    let status = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id,
            config: Some(config),
            sandbox_config: Some(sandbox_config),
        })
        .await
        .expect_err("a container with nothing to run should be rejected");
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
    assert!(
        status.message().contains("no command specified"),
        "{status:?}"
    );
    let bundles_after: Vec<_> = std::fs::read_dir(storage.path().join("cri-bundles"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(
        bundles_before, bundles_after,
        "a rejected create must leave no bundle behind"
    );
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

    // Removing the sandbox removes its containers (and their
    // bundles) too.
    client2
        .remove_pod_sandbox(RemovePodSandboxRequest {
            pod_sandbox_id: sandbox_id,
        })
        .await
        .expect("RemovePodSandbox failed");
    let not_found = client2
        .container_status(ContainerStatusRequest {
            container_id: container_id.clone(),
            verbose: false,
        })
        .await
        .expect_err("the sandbox's container should be gone too");
    assert_eq!(not_found.code(), tonic::Code::NotFound);
    assert!(
        !bundle_dir(storage.path(), &container_id).exists(),
        "RemovePodSandbox should remove the container's bundle too"
    );
    let remaining = client2
        .list_containers(ListContainersRequest { filter: None })
        .await
        .unwrap()
        .into_inner()
        .containers;
    assert!(remaining.is_empty(), "{remaining:?}");
}

/// Polls `ContainerStatus` until the reported state matches (or a
/// deadline passes).
async fn wait_for_state(
    client: &mut RuntimeServiceClient<tonic::transport::Channel>,
    container_id: &str,
    want: ContainerState,
) -> oci_cri_types::ContainerStatus {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        let status = client
            .container_status(ContainerStatusRequest {
                container_id: container_id.to_string(),
                verbose: false,
            })
            .await
            .expect("ContainerStatus failed")
            .into_inner()
            .status
            .expect("status should be present");
        if status.state == want as i32 {
            return status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "container {container_id} never reached {want:?}; last status: {status:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A real, started container (0238): `/bin/true` runs to completion,
/// and the reported status carries a real pid-backed lifecycle —
/// RUNNING (or already EXITED for something this fast), then EXITED
/// with a real exit code 0, `Completed`, and real timestamps. A
/// second start of the same container is real cri-o's own "is not in
/// created state" error.
#[tokio::test]
async fn start_runs_a_real_container_to_completion() {
    let Some((_storage, _socket, _server, mut client, sandbox_id, sandbox_config)) = setup().await
    else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };

    let mut config = container_config("runs-true", 0);
    config.command = vec!["/bin/true".to_string()];
    let container_id = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id.clone(),
            config: Some(config),
            sandbox_config: Some(sandbox_config.clone()),
        })
        .await
        .unwrap()
        .into_inner()
        .container_id;

    client
        .start_container(oci_cri_types::StartContainerRequest {
            container_id: container_id.clone(),
        })
        .await
        .expect("StartContainer failed");

    let status = wait_for_state(&mut client, &container_id, ContainerState::ContainerExited).await;
    assert_eq!(status.exit_code, 0, "{status:?}");
    assert_eq!(status.reason, "Completed", "{status:?}");
    assert!(status.started_at > 0, "{status:?}");
    assert!(status.finished_at >= status.started_at, "{status:?}");

    // Starting it again: real cri-o's own error, verbatim shape.
    let err = client
        .start_container(oci_cri_types::StartContainerRequest {
            container_id: container_id.clone(),
        })
        .await
        .expect_err("a second start should be rejected");
    assert_eq!(err.code(), tonic::Code::FailedPrecondition);
    assert!(err.message().contains("is not in created state"), "{err:?}");
}

/// A long-running container is genuinely RUNNING (live pid), then
/// `StopContainer` with a grace period ends it via SIGTERM — proven
/// by a real TERM trap inside the container (its own chosen exit
/// code comes back, not SIGKILL's), since a handler-less pid 1
/// simply *ignores* SIGTERM (a real kernel rule for init processes;
/// real `docker stop` on a handler-less pid 1 waits out its whole
/// grace period and SIGKILLs for the exact same reason) — and a
/// second stop is a silent, idempotent success. Stopping a
/// never-started container settles it as exited with no recorded
/// code (reported -1), real cri-o's own no-living-process path.
#[tokio::test]
async fn stop_terminates_a_running_container_and_is_idempotent() {
    let Some((storage, _socket, _server, mut client, sandbox_id, sandbox_config)) = setup().await
    else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };

    let mut config = container_config("sleeper", 0);
    // The TERM trap makes pid 1 exit voluntarily with its own code
    // (see this test's own doc comment). Two real subtleties, both
    // found the hard way wiring this test up:
    //
    // * A *foreground* sleep loop, deliberately: busybox `sh`
    //   redirects a backgrounded job's stdin from `/dev/null`, which
    //   this project's own containers don't populate in `/dev` yet (a
    //   `sleep 300 & wait` variant exited 0 instantly because the
    //   background spawn itself failed). The trap still runs promptly
    //   (busybox delivers it once the current foreground `sleep 1`
    //   returns, well inside the stop grace period).
    // * `touch /ready` *after* the trap: a pid-namespace init that
    //   hasn't installed a handler yet silently *discards* SIGTERM
    //   from the parent namespace (a real kernel rule for init
    //   processes, not a bug anywhere) — and `RUNNING` is reported
    //   from the moment the pid exists, which is before the container
    //   has even exec'd. Stopping the instant `RUNNING` appears can
    //   therefore race the trap installation; the test waits for the
    //   container's own real signal (`/ready` in its rootfs, visible
    //   on the host through the bundle) before stopping.
    config.command = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "trap 'exit 42' TERM; touch /ready; while true; do sleep 1; done".to_string(),
    ];
    let container_id = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id.clone(),
            config: Some(config),
            sandbox_config: Some(sandbox_config.clone()),
        })
        .await
        .unwrap()
        .into_inner()
        .container_id;
    client
        .start_container(oci_cri_types::StartContainerRequest {
            container_id: container_id.clone(),
        })
        .await
        .expect("StartContainer failed");
    let status = wait_for_state(&mut client, &container_id, ContainerState::ContainerRunning).await;
    assert!(status.started_at > 0);
    assert_eq!(status.finished_at, 0, "still running: {status:?}");

    // Wait for the container's own trap-installed signal (see the
    // command's own comment above) before stopping.
    let ready = bundle_dir(storage.path(), &container_id).join("rootfs/ready");
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while !ready.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "container never touched /ready"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    client
        .stop_container(oci_cri_types::StopContainerRequest {
            container_id: container_id.clone(),
            timeout: 10,
        })
        .await
        .expect("StopContainer failed");
    let status = wait_for_state(&mut client, &container_id, ContainerState::ContainerExited).await;
    assert_eq!(
        status.exit_code, 42,
        "the TERM trap's own exit code proves the graceful path ran: {status:?}"
    );
    assert_eq!(status.reason, "Error", "{status:?}");

    // Idempotent second stop.
    client
        .stop_container(oci_cri_types::StopContainerRequest {
            container_id: container_id.clone(),
            timeout: 10,
        })
        .await
        .expect("a second StopContainer should silently succeed");

    // Stopping a never-started container settles it (no exit code
    // was ever produced, reported as -1).
    let mut config = container_config("never-started", 0);
    config.command = vec!["/bin/true".to_string()];
    let created_only = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id,
            config: Some(config),
            sandbox_config: Some(sandbox_config),
        })
        .await
        .unwrap()
        .into_inner()
        .container_id;
    client
        .stop_container(oci_cri_types::StopContainerRequest {
            container_id: created_only.clone(),
            timeout: 5,
        })
        .await
        .expect("stopping a created container should succeed");
    let status = wait_for_state(&mut client, &created_only, ContainerState::ContainerExited).await;
    assert_eq!(status.exit_code, -1, "{status:?}");
    assert!(status.finished_at > 0, "{status:?}");
}

/// `RemoveContainer` of a running container is forceful (the proto's
/// own contract): the real process is killed, the record and bundle
/// removed — and the state filter sees a genuinely reconciled view
/// (a RUNNING record whose process exited lists as EXITED).
#[tokio::test]
async fn remove_forcefully_kills_a_running_container() {
    let Some((storage, _socket, _server, mut client, sandbox_id, sandbox_config)) = setup().await
    else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };

    let mut config = container_config("doomed", 0);
    config.command = vec!["/bin/sleep".to_string(), "300".to_string()];
    let container_id = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id.clone(),
            config: Some(config),
            sandbox_config: Some(sandbox_config.clone()),
        })
        .await
        .unwrap()
        .into_inner()
        .container_id;
    client
        .start_container(oci_cri_types::StartContainerRequest {
            container_id: container_id.clone(),
        })
        .await
        .unwrap();
    wait_for_state(&mut client, &container_id, ContainerState::ContainerRunning).await;

    client
        .remove_container(RemoveContainerRequest {
            container_id: container_id.clone(),
        })
        .await
        .expect("forceful RemoveContainer of a running container should succeed");
    assert!(
        !bundle_dir(storage.path(), &container_id).exists(),
        "bundle should be gone"
    );
    let not_found = client
        .container_status(ContainerStatusRequest {
            container_id,
            verbose: false,
        })
        .await
        .expect_err("removed container should be gone");
    assert_eq!(not_found.code(), tonic::Code::NotFound);
}

/// `StopPodSandbox` forcibly terminates the sandbox's own running
/// containers (the proto's contract, real cri-o's own
/// `stopPodSandbox` loop).
#[tokio::test]
async fn stop_pod_sandbox_terminates_its_running_containers() {
    let Some((_storage, _socket, _server, mut client, sandbox_id, sandbox_config)) = setup().await
    else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };

    let mut config = container_config("pod-sleeper", 0);
    config.command = vec!["/bin/sleep".to_string(), "300".to_string()];
    let container_id = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id.clone(),
            config: Some(config),
            sandbox_config: Some(sandbox_config.clone()),
        })
        .await
        .unwrap()
        .into_inner()
        .container_id;
    client
        .start_container(oci_cri_types::StartContainerRequest {
            container_id: container_id.clone(),
        })
        .await
        .unwrap();
    wait_for_state(&mut client, &container_id, ContainerState::ContainerRunning).await;

    client
        .stop_pod_sandbox(StopPodSandboxRequest {
            pod_sandbox_id: sandbox_id,
        })
        .await
        .expect("StopPodSandbox failed");
    let status = wait_for_state(&mut client, &container_id, ContainerState::ContainerExited).await;
    assert_eq!(
        status.exit_code,
        128 + 9,
        "the sandbox stop is forceful (SIGKILL): {status:?}"
    );
}

/// `ExecSync` (`docs/design/0240`) runs a real command inside a real
/// running container: stdout and stderr come back separately, the
/// command's own exit code comes back verbatim, a timeout is real
/// cri-o's own *successful* `-1`/"command timed out" response shape
/// (never a gRPC error — kubelet's prober checks the exit code), and
/// the non-exec-able states are real `NotFound`s.
#[tokio::test]
async fn exec_sync_runs_commands_in_a_running_container() {
    let Some((_storage, _socket, _server, mut client, sandbox_id, sandbox_config)) = setup().await
    else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };

    let mut config = container_config("exec-target", 0);
    config.command = vec!["/bin/sleep".to_string(), "300".to_string()];
    let container_id = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id.clone(),
            config: Some(config),
            sandbox_config: Some(sandbox_config.clone()),
        })
        .await
        .unwrap()
        .into_inner()
        .container_id;

    // Exec before start: our created containers have no process at
    // all (0236), so this is a real NotFound.
    eprintln!("PH A {:?}", std::time::Instant::now());
    let err = client
        .exec_sync(oci_cri_types::ExecSyncRequest {
            container_id: container_id.clone(),
            cmd: vec!["/bin/true".to_string()],
            timeout: 0,
        })
        .await
        .expect_err("exec into a never-started container should fail");
    assert_eq!(err.code(), tonic::Code::NotFound);

    client
        .start_container(oci_cri_types::StartContainerRequest {
            container_id: container_id.clone(),
        })
        .await
        .unwrap();
    wait_for_state(&mut client, &container_id, ContainerState::ContainerRunning).await;
    eprintln!("PH B(started) {:?}", std::time::Instant::now());

    // Real output on both streams, and the command's own exit code.
    let response = client
        .exec_sync(oci_cri_types::ExecSyncRequest {
            container_id: container_id.clone(),
            cmd: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "echo out-hi; echo err-hi 1>&2; exit 7".to_string(),
            ],
            timeout: 0,
        })
        .await
        .expect("ExecSync failed")
        .into_inner();
    assert_eq!(String::from_utf8_lossy(&response.stdout), "out-hi\n");
    assert_eq!(String::from_utf8_lossy(&response.stderr), "err-hi\n");
    assert_eq!(response.exit_code, 7);
    eprintln!("PH C(exec1 done) {:?}", std::time::Instant::now());

    // The exec genuinely ran *inside* the container: its /proc is the
    // container's own pid namespace, where the sleep init is pid 1.
    let response = client
        .exec_sync(oci_cri_types::ExecSyncRequest {
            container_id: container_id.clone(),
            cmd: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "cat /proc/1/cmdline | tr '\\0' ' '".to_string(),
            ],
            timeout: 0,
        })
        .await
        .expect("ExecSync failed")
        .into_inner();
    assert_eq!(response.exit_code, 0, "{response:?}");
    assert_eq!(
        String::from_utf8_lossy(&response.stdout).trim(),
        "/bin/sleep 300",
        "pid 1 inside the exec's own view must be the container init"
    );
    eprintln!("PH D(exec2 done) {:?}", std::time::Instant::now());

    // Timeout: real cri-o's own successful -1/"command timed out"
    // shape, and it actually returns promptly rather than sleeping 30s.
    let started = std::time::Instant::now();
    let response = client
        .exec_sync(oci_cri_types::ExecSyncRequest {
            container_id: container_id.clone(),
            cmd: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "sleep 30".to_string(),
            ],
            timeout: 1,
        })
        .await
        .expect("a timed-out ExecSync must still be a successful response")
        .into_inner();
    assert_eq!(response.exit_code, -1, "{response:?}");
    assert_eq!(
        String::from_utf8_lossy(&response.stderr),
        "command timed out"
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the timeout must actually cut the command short"
    );
    eprintln!("PH E(timeout done) {:?}", std::time::Instant::now());

    // An empty command is real cri-o's own verbatim error.
    let err = client
        .exec_sync(oci_cri_types::ExecSyncRequest {
            container_id: container_id.clone(),
            cmd: Vec::new(),
            timeout: 0,
        })
        .await
        .expect_err("an empty exec command should be rejected");
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert!(
        err.message().contains("exec command cannot be empty"),
        "{err:?}"
    );

    // Unknown container: NotFound.
    let err = client
        .exec_sync(oci_cri_types::ExecSyncRequest {
            container_id: "deadbeef".repeat(8),
            cmd: vec!["/bin/true".to_string()],
            timeout: 0,
        })
        .await
        .expect_err("exec into an unknown container should fail");
    assert_eq!(err.code(), tonic::Code::NotFound);
    eprintln!("PH F(stop begins) {:?}", std::time::Instant::now());

    // Exec into an exited container: NotFound too.
    client
        .stop_container(oci_cri_types::StopContainerRequest {
            container_id: container_id.clone(),
            timeout: 0,
        })
        .await
        .unwrap();
    wait_for_state(&mut client, &container_id, ContainerState::ContainerExited).await;
    eprintln!("PH G(stopped) {:?}", std::time::Instant::now());
    let err = client
        .exec_sync(oci_cri_types::ExecSyncRequest {
            container_id,
            cmd: vec!["/bin/true".to_string()],
            timeout: 0,
        })
        .await
        .expect_err("exec into an exited container should fail");
    assert_eq!(err.code(), tonic::Code::NotFound);
}
