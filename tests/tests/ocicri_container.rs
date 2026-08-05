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

use oci_cri_types::image_service_client::ImageServiceClient;
use oci_cri_types::runtime_service_client::RuntimeServiceClient;
use oci_cri_types::{
    ContainerConfig as CriContainerConfig, ContainerFilter, ContainerMetadata, ContainerState,
    ContainerStateValue, ContainerStatusRequest, CreateContainerRequest, DnsConfig, IdMapping,
    ImageSpec, ListContainersRequest, Mount, MountPropagation, PodSandboxConfig,
    PodSandboxMetadata, RemoveContainerRequest, RemoveImageRequest, RemovePodSandboxRequest,
    RunPodSandboxRequest, StopPodSandboxRequest,
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

/// Same real Unix socket [`connect`] uses, a second, independent
/// client stub for `ImageService` -- both services are served by the
/// exact same `ocicri` process/listener, matching real cri-o's own
/// single-socket, multi-service shape.
async fn connect_image_service(
    socket_path: std::path::PathBuf,
) -> ImageServiceClient<tonic::transport::Channel> {
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

/// `PodSandboxConfig.hostname` wiring (0292): a real, previously-
/// missing per-container UTS setting — every CRI container used to
/// silently report the shared spec template's own hardcoded
/// `"ocirun"` hostname regardless of the pod's own real name/config.
/// Matches real cri-o's own `getHostname` exactly (checked directly
/// against `~/git/cri-o/server/sandbox_run.go`): the sandbox config's
/// own `hostname` if non-empty, else the sandbox id's own first 12
/// hex chars — verified both ways, plus the matching `HOSTNAME=`
/// process env var real cri-o's own `AddProcessEnv` call site also
/// sets.
#[tokio::test]
async fn create_container_wires_the_pod_sandboxs_own_hostname() {
    let Some((storage, _socket, _server, mut client, sandbox_id, mut sandbox_config)) =
        setup().await
    else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    sandbox_config.hostname = "custom-pod-hostname".to_string();

    let container_id = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id.clone(),
            config: Some(container_config("hostname-explicit", 0)),
            sandbox_config: Some(sandbox_config.clone()),
        })
        .await
        .expect("CreateContainer failed")
        .into_inner()
        .container_id;
    let spec: serde_json::Value = serde_json::from_slice(
        &std::fs::read(bundle_dir(storage.path(), &container_id).join("config.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(spec["hostname"], serde_json::json!("custom-pod-hostname"));
    assert!(
        spec["process"]["env"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "HOSTNAME=custom-pod-hostname"),
        "{spec:?}"
    );

    // An empty `sandbox_config.hostname` (real kubelet very commonly
    // never sets it at all) falls back to the sandbox id's own first
    // 12 hex chars, matching real cri-o's own non-host-network
    // default exactly -- this project has no host-networking concept
    // for a sandbox at all, so that's the only real fallback branch
    // reachable here.
    let mut no_hostname_config = sandbox_config.clone();
    no_hostname_config.hostname = String::new();
    let fallback_id = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id.clone(),
            config: Some(container_config("hostname-fallback", 0)),
            sandbox_config: Some(no_hostname_config),
        })
        .await
        .expect("CreateContainer failed")
        .into_inner()
        .container_id;
    let fallback_spec: serde_json::Value = serde_json::from_slice(
        &std::fs::read(bundle_dir(storage.path(), &fallback_id).join("config.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        fallback_spec["hostname"],
        serde_json::json!(sandbox_id[..12]),
        "{fallback_spec:?}"
    );
}

/// A real, synthesized `/etc/hosts` (0296): a previously-missing
/// primitive `bundle.rs`'s own module doc comment had explicitly
/// named as out of scope until now, closed by reusing the exact same
/// `oci_runtime_core::etc_hosts::write_etc_hosts` `ociman run` already
/// established (`0147`) — this project sets up no container
/// networking of its own at all, so the sandbox's own real hostname
/// maps to `127.0.0.1`, matching real cri-o's own non-host-network
/// default and `ociman run`'s own `--network=none`-shaped case
/// exactly.
#[tokio::test]
async fn create_container_writes_a_real_etc_hosts_mapping_its_own_hostname_to_loopback() {
    let Some((storage, _socket, _server, mut client, sandbox_id, mut sandbox_config)) =
        setup().await
    else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    sandbox_config.hostname = "hosts-test-hostname".to_string();

    let container_id = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id.clone(),
            config: Some(container_config("hosts-test", 0)),
            sandbox_config: Some(sandbox_config),
        })
        .await
        .expect("CreateContainer failed")
        .into_inner()
        .container_id;

    let hosts_content =
        std::fs::read_to_string(bundle_dir(storage.path(), &container_id).join("rootfs/etc/hosts"))
            .expect("a real /etc/hosts should have been written into the extracted rootfs");
    assert!(
        hosts_content.contains("127.0.0.1\tlocalhost"),
        "{hosts_content:?}"
    );
    assert!(
        hosts_content.contains("127.0.0.1\thosts-test-hostname"),
        "{hosts_content:?}"
    );
}

/// A real `/etc/hostname` file, containing the exact same value
/// passed to `sethostname(2)` (`spec.hostname`) -- a real,
/// previously-unnoticed gap found while researching `ociman build
/// --no-hostname` (`docs/design/0459`), closed here in the same
/// increment that adds it for `ociman run`/`create` too, reusing the
/// same new `oci_runtime_core::etc_hosts::write_etc_hostname`
/// primitive.
#[tokio::test]
async fn create_container_writes_a_real_etc_hostname_matching_the_sandboxs_own_hostname() {
    let Some((storage, _socket, _server, mut client, sandbox_id, mut sandbox_config)) =
        setup().await
    else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    sandbox_config.hostname = "hostname-file-test".to_string();

    let container_id = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id.clone(),
            config: Some(container_config("hostname-file-test", 0)),
            sandbox_config: Some(sandbox_config),
        })
        .await
        .expect("CreateContainer failed")
        .into_inner()
        .container_id;

    let hostname_content = std::fs::read_to_string(
        bundle_dir(storage.path(), &container_id).join("rootfs/etc/hostname"),
    )
    .expect("a real /etc/hostname should have been written into the extracted rootfs");
    assert_eq!(hostname_content, "hostname-file-test\n");
}

/// A real `/etc/resolv.conf` (0297, closing `0296`'s own "still
/// ahead"), matching real cri-o's own `ParseDNSOptions` exactly
/// (checked directly against `~/git/cri-o/internal/lib/sandbox/
/// infra.go`): an explicit `PodSandboxConfig.dns_config` is
/// synthesized from scratch, `search`/`nameserver`/`options` in that
/// real order.
#[tokio::test]
async fn create_container_writes_a_real_resolv_conf_from_explicit_dns_config() {
    let Some((storage, _socket, _server, mut client, sandbox_id, mut sandbox_config)) =
        setup().await
    else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    sandbox_config.dns_config = Some(DnsConfig {
        servers: vec!["10.0.0.1".to_string(), "10.0.0.2".to_string()],
        searches: vec!["example.com".to_string()],
        options: vec!["ndots:5".to_string()],
    });

    let container_id = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id.clone(),
            config: Some(container_config("resolv-explicit-test", 0)),
            sandbox_config: Some(sandbox_config),
        })
        .await
        .expect("CreateContainer failed")
        .into_inner()
        .container_id;

    let resolv_content = std::fs::read_to_string(
        bundle_dir(storage.path(), &container_id).join("rootfs/etc/resolv.conf"),
    )
    .expect("a real /etc/resolv.conf should have been written into the extracted rootfs");
    assert_eq!(
        resolv_content,
        "search example.com\nnameserver 10.0.0.1\nnameserver 10.0.0.2\noptions ndots:5\n"
    );
}

/// `PodSandboxConfig.linux.sysctls` (`docs/design/0396`): a real,
/// sandbox-(pod-)level CRI concept, genuinely applied to a real
/// started container's own kernel parameters — a real, previously-
/// silent gap this closes: `linux.sysctl` stayed empty on every CRI
/// container regardless of what a pod's own `securityContext.
/// sysctls` actually requested. Uses a real `kernel.*` key that only
/// needs an IPC namespace, always present for every container this
/// project's own `ocicri` launches.
#[tokio::test]
async fn create_container_applies_the_sandboxs_own_sysctls_to_a_real_running_container() {
    let Some((_storage, _socket, _server, mut client, sandbox_id, mut sandbox_config)) =
        setup().await
    else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    sandbox_config.linux = Some(oci_cri_types::LinuxPodSandboxConfig {
        sysctls: HashMap::from([("kernel.shmmax".to_string(), "8000000".to_string())]),
        ..Default::default()
    });

    let mut config = container_config("sysctl-test", 0);
    config.command = vec!["/bin/sleep".to_string(), "300".to_string()];
    let container_id = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id.clone(),
            config: Some(config),
            sandbox_config: Some(sandbox_config),
        })
        .await
        .expect("CreateContainer failed")
        .into_inner()
        .container_id;
    client
        .start_container(oci_cri_types::StartContainerRequest {
            container_id: container_id.clone(),
        })
        .await
        .unwrap();
    wait_for_state(&mut client, &container_id, ContainerState::ContainerRunning).await;

    let response = client
        .exec_sync(oci_cri_types::ExecSyncRequest {
            container_id: container_id.clone(),
            cmd: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "cat /proc/sys/kernel/shmmax".to_string(),
            ],
            timeout: 0,
        })
        .await
        .expect("ExecSync failed")
        .into_inner();
    assert_eq!(response.exit_code, 0, "{response:?}");
    assert_eq!(String::from_utf8_lossy(&response.stdout).trim(), "8000000");

    client
        .stop_container(oci_cri_types::StopContainerRequest {
            container_id,
            timeout: 0,
        })
        .await
        .unwrap();
}

/// `ContainerConfig.linux.resources.oom_score_adj` (`docs/design/
/// 0400`): a real, explicit non-zero value must land in the started
/// container's own real `/proc/1/oom_score_adj` -- previously read
/// nowhere at all, so a pod's own explicit request was silently
/// dropped. Read via `/proc/1/` (the container's own init process,
/// inside its own pid namespace), not `/proc/self/` -- unlike
/// `ociman run --oom-score-adj`'s own equivalent test (where the
/// timed command genuinely *is* the container's own init process,
/// `execve`d in place, which inherits whatever `oom_score_adj` was
/// already set), `ExecSync` here runs a *separate*, freshly forked
/// process only joining the container's namespaces -- it was never
/// the one `oci_runtime_core::oom::apply` adjusted at container-
/// creation time, so its own `/proc/self` would read back `0`
/// regardless of whether this feature actually works. Needs no live
/// cgroup at all (unlike `resources`' other fields), so this doesn't
/// need the `systemd_user_session_available` gate `create_container_
/// resources_take_effect_at_creation_without_a_later_update_call`
/// needs.
#[tokio::test]
async fn create_container_oom_score_adj_sets_a_real_value() {
    let Some((_storage, _socket, _server, mut client, sandbox_id, sandbox_config)) = setup().await
    else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };

    let mut config = container_config("oom-score-adj-test", 0);
    config.command = vec!["/bin/sleep".to_string(), "300".to_string()];
    config.linux = Some(oci_cri_types::LinuxContainerConfig {
        resources: Some(oci_cri_types::LinuxContainerResources {
            oom_score_adj: 500,
            ..Default::default()
        }),
        ..Default::default()
    });
    let container_id = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id.clone(),
            config: Some(config),
            sandbox_config: Some(sandbox_config),
        })
        .await
        .expect("CreateContainer failed")
        .into_inner()
        .container_id;
    client
        .start_container(oci_cri_types::StartContainerRequest {
            container_id: container_id.clone(),
        })
        .await
        .unwrap();
    wait_for_state(&mut client, &container_id, ContainerState::ContainerRunning).await;

    let response = client
        .exec_sync(oci_cri_types::ExecSyncRequest {
            container_id: container_id.clone(),
            cmd: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "cat /proc/1/oom_score_adj".to_string(),
            ],
            timeout: 0,
        })
        .await
        .expect("ExecSync failed")
        .into_inner();
    assert_eq!(response.exit_code, 0, "{response:?}");
    assert_eq!(String::from_utf8_lossy(&response.stdout).trim(), "500");

    client
        .stop_container(oci_cri_types::StopContainerRequest {
            container_id,
            timeout: 0,
        })
        .await
        .unwrap();
}

/// With no `dns_config` at all (real kubelet's own common case for a
/// pod with no special DNS policy, and `crictl`'s own bare default),
/// the real host's own `/etc/resolv.conf` is copied verbatim into the
/// container — meaningful, not just cosmetic, precisely because this
/// project's own containers already share the host's real network
/// namespace unmodified, so the host's own real nameservers genuinely
/// are reachable from inside the container too.
#[tokio::test]
async fn create_container_falls_back_to_a_real_copy_of_the_hosts_own_resolv_conf() {
    let Some((storage, _socket, _server, mut client, sandbox_id, sandbox_config)) = setup().await
    else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };

    let container_id = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id.clone(),
            config: Some(container_config("resolv-fallback-test", 0)),
            sandbox_config: Some(sandbox_config),
        })
        .await
        .expect("CreateContainer failed")
        .into_inner()
        .container_id;

    let resolv_content = std::fs::read_to_string(
        bundle_dir(storage.path(), &container_id).join("rootfs/etc/resolv.conf"),
    )
    .expect("a real /etc/resolv.conf should have been written into the extracted rootfs");
    let host_resolv_content = std::fs::read_to_string("/etc/resolv.conf")
        .expect("this real dev/CI host should have a real /etc/resolv.conf of its own");
    assert_eq!(resolv_content, host_resolv_content);
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
    // `HOSTNAME=` last (0292): `pod_config`'s own default leaves
    // `hostname` empty, so it falls back to the sandbox id's own
    // first 12 hex chars.
    assert_eq!(
        env,
        vec![
            "FROM_IMAGE=1".to_string(),
            "FROM_KUBE=2".to_string(),
            format!("HOSTNAME={}", &sandbox_id[..12]),
        ]
    );

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

/// `StreamContainers` (`CRIListStreaming`, `docs/design/0253`) reports
/// the exact same items `ListContainers` does — in one message here
/// (far fewer than real cri-o's own 3000-item chunk size), honoring a
/// filter identically — and streams zero messages (EOF immediately)
/// for an empty sandbox, matching real cri-o's own chunking loop
/// simply never iterating. Completes the `CRIListStreaming` family
/// `StreamPodSandboxes`/`StreamImages` already started (0234).
#[tokio::test]
async fn stream_containers_matches_list_and_streams_nothing_when_empty() {
    let Some((_storage, _socket, _server, mut client, sandbox_id, sandbox_config)) = setup().await
    else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };

    // Empty sandbox: a real, successful stream with zero messages.
    let mut empty_stream = client
        .stream_containers(oci_cri_types::StreamContainersRequest { filter: None })
        .await
        .expect("StreamContainers on an empty sandbox should succeed")
        .into_inner();
    assert!(
        empty_stream
            .message()
            .await
            .expect("stream should end cleanly")
            .is_none(),
        "an empty sandbox should stream zero messages before EOF"
    );

    let first = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id.clone(),
            config: Some(container_config("stream-a", 0)),
            sandbox_config: Some(sandbox_config.clone()),
        })
        .await
        .unwrap()
        .into_inner()
        .container_id;
    let second = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id.clone(),
            config: Some(container_config("stream-b", 0)),
            sandbox_config: Some(sandbox_config.clone()),
        })
        .await
        .unwrap()
        .into_inner()
        .container_id;

    // Unfiltered: the exact same containers ListContainers reports,
    // in one message.
    let listed = client
        .list_containers(ListContainersRequest { filter: None })
        .await
        .unwrap()
        .into_inner()
        .containers;
    let mut stream = client
        .stream_containers(oci_cri_types::StreamContainersRequest { filter: None })
        .await
        .expect("StreamContainers failed")
        .into_inner();
    let mut streamed = Vec::new();
    while let Some(response) = stream.message().await.expect("stream should end cleanly") {
        streamed.extend(response.containers);
    }
    assert_eq!(streamed, listed, "stream and list must report identically");
    assert_eq!(streamed.len(), 2);

    // A label filter behaves identically to the list RPC's own.
    let mut filtered_stream = client
        .stream_containers(oci_cri_types::StreamContainersRequest {
            filter: Some(ContainerFilter {
                label_selector: HashMap::from([("app".to_string(), "stream-a".to_string())]),
                ..Default::default()
            }),
        })
        .await
        .expect("filtered StreamContainers failed")
        .into_inner();
    let mut by_label = Vec::new();
    while let Some(response) = filtered_stream
        .message()
        .await
        .expect("stream should end cleanly")
    {
        by_label.extend(response.containers);
    }
    assert_eq!(by_label.len(), 1, "{by_label:?}");
    assert_eq!(by_label[0].id, first);
    assert_ne!(by_label[0].id, second);
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

/// Same real, reachable-`systemd --user`-session probe
/// `ociman_stats.rs`/`ociman_top.rs`'s own tests use — without one,
/// this project's launches fall back to no cgroup at all (the
/// documented rootless no-D-Bus fallback), and there is nothing for
/// the stats RPCs to read.
fn systemd_user_session_available() -> bool {
    Command::new("systemctl")
        .args(["--user", "is-system-running"])
        .output()
        .is_ok_and(|out| !out.stdout.is_empty())
}

/// `ContainerStats`/`ListContainerStats`/`StreamContainerStats`
/// (`docs/design/0241`): real, live cgroup-backed usage for a running
/// container — real cri-o's own cgroup-v2 formulas via the same
/// shared `oci_runtime_core::cgroups` readers `ociman stats` uses —
/// with created/exited containers honestly absent rather than
/// fabricated zero rows, and an unknown ID a real `NotFound`.
#[tokio::test]
async fn container_stats_report_real_cgroup_usage() {
    let Some((storage, _socket, _server, mut client, sandbox_id, sandbox_config)) = setup().await
    else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    if !systemd_user_session_available() {
        eprintln!("skipping: no reachable `systemd --user` session (containers get no cgroup)");
        return;
    }

    // A created-but-never-started container: no stats, honestly.
    let created_only = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id.clone(),
            config: Some(container_config("stats-created", 0)),
            sandbox_config: Some(sandbox_config.clone()),
        })
        .await
        .unwrap()
        .into_inner()
        .container_id;
    let response = client
        .container_stats(oci_cri_types::ContainerStatsRequest {
            container_id: created_only.clone(),
        })
        .await
        .expect("stats of a created container should be a real response")
        .into_inner();
    assert!(
        response.stats.is_none(),
        "a container with no live cgroup gets no stats: {response:?}"
    );

    // A really-running container: real numbers.
    let mut config = container_config("stats-runner", 0);
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

    let stats = client
        .container_stats(oci_cri_types::ContainerStatsRequest {
            container_id: container_id.clone(),
        })
        .await
        .expect("ContainerStats failed")
        .into_inner()
        .stats
        .expect("a running container should have real stats");
    let attributes = stats.attributes.expect("attributes should be present");
    assert_eq!(attributes.id, container_id);
    assert_eq!(
        attributes.labels.get("app"),
        Some(&"stats-runner".to_string())
    );
    let cpu = stats.cpu.expect("cpu usage should be present");
    assert!(cpu.timestamp > 0);
    assert!(
        cpu.usage_core_nano_seconds.is_some(),
        "usage_core_nano_seconds should be a real reading: {cpu:?}"
    );
    let memory = stats.memory.expect("memory usage should be present");
    let working_set = memory.working_set_bytes.as_ref().map_or(0, |v| v.value);
    assert!(
        working_set > 0,
        "a live sleep process has real memory: {memory:?}"
    );
    assert!(
        memory.usage_bytes.as_ref().map_or(0, |v| v.value) >= working_set,
        "raw usage includes what working-set subtracts: {memory:?}"
    );
    let writable = stats.writable_layer.expect("writable layer present");
    assert!(
        writable.used_bytes.as_ref().map_or(0, |v| v.value) > 0,
        "the extracted rootfs has real bytes: {writable:?}"
    );
    assert!(
        writable
            .fs_id
            .as_ref()
            .is_some_and(|f| f.mountpoint.starts_with(storage.path().to_str().unwrap())),
        "the mountpoint is the real bundle rootfs: {writable:?}"
    );

    // List: only the running container appears (the created-only one
    // has no live cgroup), and the sandbox filter behaves like the
    // container list's own.
    let listed = client
        .list_container_stats(oci_cri_types::ListContainerStatsRequest { filter: None })
        .await
        .expect("ListContainerStats failed")
        .into_inner()
        .stats;
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(
        listed[0].attributes.as_ref().unwrap().id,
        container_id,
        "only the running container has stats"
    );
    let by_sandbox = client
        .list_container_stats(oci_cri_types::ListContainerStatsRequest {
            filter: Some(oci_cri_types::ContainerStatsFilter {
                pod_sandbox_id: sandbox_id[..13].to_string(),
                ..Default::default()
            }),
        })
        .await
        .unwrap()
        .into_inner()
        .stats;
    assert_eq!(by_sandbox.len(), 1, "{by_sandbox:?}");

    // The streaming sibling reports the same set (0234's chunking).
    let mut stream = client
        .stream_container_stats(oci_cri_types::StreamContainerStatsRequest { filter: None })
        .await
        .expect("StreamContainerStats failed")
        .into_inner();
    let mut streamed = Vec::new();
    while let Some(response) = stream.message().await.expect("stream should end cleanly") {
        streamed.extend(response.container_stats);
    }
    assert_eq!(streamed.len(), 1);
    assert_eq!(streamed[0].attributes.as_ref().unwrap().id, container_id);

    // Unknown ID: a real NotFound (real cri-o's own single-stats
    // error), and cleanup.
    let err = client
        .container_stats(oci_cri_types::ContainerStatsRequest {
            container_id: "deadbeef".repeat(8),
        })
        .await
        .expect_err("stats of an unknown container should fail");
    assert_eq!(err.code(), tonic::Code::NotFound);
    client
        .stop_container(oci_cri_types::StopContainerRequest {
            container_id,
            timeout: 0,
        })
        .await
        .unwrap();
}

/// `UpdateContainerResources` (`docs/design/0251`): real, live cgroup
/// writes for a running container via the same shared
/// `oci_runtime_core::cgroups::plan_resources`/`apply` pair `ociman
/// update`/`ocirun update` already use — checked directly against the
/// real cgroup files afterward, the same way `ociman_update.rs`'s own
/// `update_changes_the_real_live_cgroup_limits_of_a_running_container`
/// does. A `Created` (never-started) container has no live cgroup at
/// all in this project's own model, so it's a clear `FailedPrecondition`
/// rather than a silent no-op; an unknown ID is a real `NotFound`.
#[tokio::test]
async fn update_container_resources_changes_the_real_live_cgroup() {
    let Some((_storage, _socket, _server, mut client, sandbox_id, sandbox_config)) = setup().await
    else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    if !systemd_user_session_available() {
        eprintln!("skipping: no reachable `systemd --user` session (containers get no cgroup)");
        return;
    }

    // Unknown ID: a real NotFound, before ever touching a real
    // container.
    let err = client
        .update_container_resources(oci_cri_types::UpdateContainerResourcesRequest {
            container_id: "deadbeef".repeat(8),
            linux: Some(oci_cri_types::LinuxContainerResources {
                memory_limit_in_bytes: 64 * 1024 * 1024,
                ..Default::default()
            }),
            ..Default::default()
        })
        .await
        .expect_err("updating an unknown container should fail");
    assert_eq!(err.code(), tonic::Code::NotFound);

    // A created-but-never-started container: no live cgroup, so this
    // is a clear precondition failure, not a silent no-op.
    let created_only = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id.clone(),
            config: Some(container_config("update-created", 0)),
            sandbox_config: Some(sandbox_config.clone()),
        })
        .await
        .unwrap()
        .into_inner()
        .container_id;
    let err = client
        .update_container_resources(oci_cri_types::UpdateContainerResourcesRequest {
            container_id: created_only,
            linux: Some(oci_cri_types::LinuxContainerResources {
                memory_limit_in_bytes: 64 * 1024 * 1024,
                ..Default::default()
            }),
            ..Default::default()
        })
        .await
        .expect_err("updating a created-only container should fail");
    assert_eq!(err.code(), tonic::Code::FailedPrecondition);

    // A really-running container: real cgroup writes.
    let mut config = container_config("update-runner", 0);
    config.command = vec!["/bin/sleep".to_string(), "300".to_string()];
    let container_id = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id.clone(),
            config: Some(config),
            sandbox_config: Some(sandbox_config),
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
        .update_container_resources(oci_cri_types::UpdateContainerResourcesRequest {
            container_id: container_id.clone(),
            linux: Some(oci_cri_types::LinuxContainerResources {
                memory_limit_in_bytes: 64 * 1024 * 1024,
                cpu_quota: 50_000,
                cpu_period: 100_000,
                // Deliberately no `cpuset_cpus`/`cpuset_mems` here:
                // the `cpuset` controller isn't always delegated into
                // a real user systemd session's own cgroup subtree
                // (`plan_cpu`'s own doc comment notes this
                // requirement directly) -- neither `ociman_update.rs`
                // nor `ocirun_update.rs` exercises it for the exact
                // same, already-established reason.
                ..Default::default()
            }),
            ..Default::default()
        })
        .await
        .expect("UpdateContainerResources failed");

    // Resolve the real cgroup directory from the record's own pid
    // (verbose ContainerStatus's info blob, the only way a test
    // outside `ocicri`'s own crate can reach its private container
    // module) and read the real files back -- not just trusting the
    // RPC's own empty, content-free success response.
    let verbose = client
        .container_status(ContainerStatusRequest {
            container_id: container_id.clone(),
            verbose: true,
        })
        .await
        .unwrap()
        .into_inner();
    let info = verbose.info.get("info").expect("verbose info under 'info'");
    let parsed: serde_json::Value = serde_json::from_str(info).unwrap();
    let pid = parsed["pid"].as_i64().expect("running container has a pid") as i32;
    let cgroup_dir =
        oci_runtime_core::cgroups::cgroup_dir_for_running_pid(Path::new("/sys/fs/cgroup"), pid)
            .expect("resolving the real cgroup for a running container");

    let memory_max = std::fs::read_to_string(cgroup_dir.join("memory.max")).unwrap();
    assert_eq!(memory_max.trim(), (64 * 1024 * 1024).to_string());
    let cpu_max = std::fs::read_to_string(cgroup_dir.join("cpu.max")).unwrap();
    assert_eq!(cpu_max.trim(), "50000 100000");

    // An absent `linux` half is a real, documented no-op (matching
    // real cri-o's own identical behavior) -- never an error.
    client
        .update_container_resources(oci_cri_types::UpdateContainerResourcesRequest {
            container_id: container_id.clone(),
            linux: None,
            ..Default::default()
        })
        .await
        .expect("an absent Linux half should be a harmless no-op");

    client
        .stop_container(oci_cri_types::StopContainerRequest {
            container_id,
            timeout: 0,
        })
        .await
        .unwrap();
}

/// `LinuxPodSandboxConfig.cgroup_parent` (`0467`, closing `0465`'s
/// own "still out of scope" note for `ocicri`) sets the real
/// transient scope's own `Slice=` unit property -- previously never
/// read at all. The scope name is deterministic (`ocicri-
/// <container_id>.scope`, `launcher.rs`'s own fixed convention), so
/// this queries it directly rather than discovering it by pattern the
/// way `ociman build`'s own equivalent test has to.
#[tokio::test]
async fn create_container_cgroup_parent_sets_the_real_systemd_scopes_own_slice_property() {
    let Some((_storage, _socket, _server, mut client, sandbox_id, mut sandbox_config)) =
        setup().await
    else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    if !systemd_user_session_available() {
        eprintln!("skipping: no reachable `systemd --user` session");
        return;
    }
    sandbox_config.linux = Some(oci_cri_types::LinuxPodSandboxConfig {
        cgroup_parent: "app.slice".to_string(),
        ..Default::default()
    });

    let mut config = container_config("cgroup-parent-test", 0);
    config.command = vec!["/bin/sleep".to_string(), "300".to_string()];
    let container_id = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id.clone(),
            config: Some(config),
            sandbox_config: Some(sandbox_config),
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

    let scope_name = format!("ocicri-{container_id}.scope");
    let show = Command::new("systemctl")
        .args(["--user", "show", &scope_name, "-p", "Slice", "--value"])
        .output()
        .expect("failed to run systemctl --user show");
    let slice = String::from_utf8_lossy(&show.stdout).trim().to_string();

    client
        .stop_container(oci_cri_types::StopContainerRequest {
            container_id,
            timeout: 0,
        })
        .await
        .unwrap();

    assert_eq!(
        slice, "app.slice",
        "expected the real systemd scope's own Slice to reflect \
         LinuxPodSandboxConfig.cgroup_parent = \"app.slice\""
    );
}

/// `ContainerConfig.linux.resources` (`docs/design/0390`): a real,
/// explicit resources request must already be in effect the moment a
/// container starts, with no separate `UpdateContainerResources` call
/// needed at all -- matching ordinary Kubernetes QoS/resource-
/// isolation expectations (kubelet normally never issues that RPC for
/// a pod without in-place vertical scaling). Previously never wired
/// in at `CreateContainer` time: a container ran completely
/// unconstrained until (if ever) a later, separate update call
/// happened to arrive. Checked the same way `update_container_
/// resources_changes_the_real_live_cgroup` checks its own identical
/// fields: real `memory.max`/`cpu.max` cgroup files read back
/// directly, not just the RPC's own content-free success response.
#[tokio::test]
async fn create_container_resources_take_effect_at_creation_without_a_later_update_call() {
    let Some((_storage, _socket, _server, mut client, sandbox_id, sandbox_config)) = setup().await
    else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    if !systemd_user_session_available() {
        eprintln!("skipping: no reachable `systemd --user` session (containers get no cgroup)");
        return;
    }

    let mut config = container_config("create-with-resources", 0);
    config.command = vec!["/bin/sleep".to_string(), "300".to_string()];
    config.linux = Some(oci_cri_types::LinuxContainerConfig {
        resources: Some(oci_cri_types::LinuxContainerResources {
            memory_limit_in_bytes: 64 * 1024 * 1024,
            cpu_quota: 50_000,
            cpu_period: 100_000,
            // Deliberately no `cpuset_cpus`/`cpuset_mems` -- the same
            // "not always delegated into a real user systemd
            // session's own cgroup subtree" reasoning `update_
            // container_resources_changes_the_real_live_cgroup`'s own
            // doc comment already establishes.
            ..Default::default()
        }),
        ..Default::default()
    });
    let container_id = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id.clone(),
            config: Some(config),
            sandbox_config: Some(sandbox_config),
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

    // No `UpdateContainerResources` call anywhere in this test: the
    // limits below must already reflect what `CreateContainer` itself
    // requested.
    let verbose = client
        .container_status(ContainerStatusRequest {
            container_id: container_id.clone(),
            verbose: true,
        })
        .await
        .unwrap()
        .into_inner();
    let info = verbose.info.get("info").expect("verbose info under 'info'");
    let parsed: serde_json::Value = serde_json::from_str(info).unwrap();
    let pid = parsed["pid"].as_i64().expect("running container has a pid") as i32;
    let cgroup_dir =
        oci_runtime_core::cgroups::cgroup_dir_for_running_pid(Path::new("/sys/fs/cgroup"), pid)
            .expect("resolving the real cgroup for a running container");

    let memory_max = std::fs::read_to_string(cgroup_dir.join("memory.max")).unwrap();
    assert_eq!(memory_max.trim(), (64 * 1024 * 1024).to_string());
    let cpu_max = std::fs::read_to_string(cgroup_dir.join("cpu.max")).unwrap();
    assert_eq!(cpu_max.trim(), "50000 100000");

    client
        .stop_container(oci_cri_types::StopContainerRequest {
            container_id,
            timeout: 0,
        })
        .await
        .unwrap();
}

/// The CRI log path (`docs/design/0242`): a container created with
/// kubelet's own `log_directory` + `log_path` convention streams its
/// stdout/stderr into a real, CRI-format log file (`<RFC3339Nano>
/// <stream> <P|F> <content>` — what `kubectl logs`/`crictl logs`
/// actually read), complete by the time the exit is observable, with
/// the joined path reported by `ContainerStatus` — and a container
/// without log config gets no file at all.
#[tokio::test]
async fn container_logs_are_written_in_the_cri_format() {
    let Some((_storage, _socket, _server, mut client, sandbox_id, mut sandbox_config)) =
        setup().await
    else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };

    let log_dir = tempfile::tempdir().unwrap();
    sandbox_config.log_directory = log_dir.path().display().to_string();

    let mut config = container_config("logger", 0);
    config.command = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "echo out-line; echo err-line 1>&2; printf no-newline".to_string(),
    ];
    // kubelet's own convention routinely nests a subdirectory.
    config.log_path = "logger/0.log".to_string();
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

    let expected_path = log_dir.path().join("logger/0.log");
    let status = client
        .container_status(ContainerStatusRequest {
            container_id: container_id.clone(),
            verbose: false,
        })
        .await
        .unwrap()
        .into_inner()
        .status
        .unwrap();
    assert_eq!(
        status.log_path,
        expected_path.display().to_string(),
        "ContainerStatus reports the joined path"
    );

    client
        .start_container(oci_cri_types::StartContainerRequest {
            container_id: container_id.clone(),
        })
        .await
        .unwrap();
    wait_for_state(&mut client, &container_id, ContainerState::ContainerExited).await;

    // The log file is complete no later than the exit is observable
    // (the launcher releases its pipe ends before recording the
    // exit) -- but the logger's own final flush is a separate
    // process; poll briefly rather than assuming perfect ordering.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let contents = loop {
        let contents = std::fs::read_to_string(&expected_path).unwrap_or_default();
        if contents.lines().count() >= 3 {
            break contents;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "log file never completed; contents so far: {contents:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };

    let lines: Vec<Vec<&str>> = contents
        .lines()
        .map(|l| l.splitn(4, ' ').collect())
        .collect();
    assert_eq!(lines.len(), 3, "{contents:?}");
    for fields in &lines {
        assert_eq!(fields.len(), 4, "{fields:?}");
        // RFC3339Nano, e.g. 2016-10-06T00:17:09.669794202Z.
        assert!(
            fields[0].len() == 30 && fields[0].ends_with('Z') && fields[0].contains('.'),
            "timestamp shape: {fields:?}"
        );
    }
    // Entries from *different* streams have no guaranteed relative
    // order (two pipes, two logger threads -- real conmon behaves
    // identically; kubelet orders by timestamp), so assert per
    // stream: within one stream, order is real.
    let stdout_entries: Vec<(&str, &str)> = lines
        .iter()
        .filter(|f| f[1] == "stdout")
        .map(|f| (f[2], f[3]))
        .collect();
    let stderr_entries: Vec<(&str, &str)> = lines
        .iter()
        .filter(|f| f[1] == "stderr")
        .map(|f| (f[2], f[3]))
        .collect();
    assert_eq!(
        stdout_entries,
        vec![("F", "out-line"), ("P", "no-newline")],
        "{contents:?}"
    );
    assert_eq!(stderr_entries, vec![("F", "err-line")], "{contents:?}");

    // A container with no log config gets no file (and an empty
    // log_path in its status), exactly as before this increment.
    let mut config = container_config("no-logs", 0);
    config.command = vec!["/bin/true".to_string()];
    let no_log_id = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id,
            config: Some(config),
            sandbox_config: Some(sandbox_config),
        })
        .await
        .unwrap()
        .into_inner()
        .container_id;
    let status = client
        .container_status(ContainerStatusRequest {
            container_id: no_log_id,
            verbose: false,
        })
        .await
        .unwrap()
        .into_inner()
        .status
        .unwrap();
    assert_eq!(status.log_path, "", "{status:?}");
}

/// `ReopenContainerLog` (`docs/design/0243`): after the log file is
/// renamed away (kubelet's own rotation), the reopen command makes
/// the logger start a fresh file at the same path — old lines stay
/// in the renamed file, new lines land in the new one. Non-running
/// and log-less containers are clear errors (cri-o's own "container
/// is not running" verbatim for the former).
#[tokio::test]
async fn reopen_container_log_rotates_to_a_fresh_file() {
    let Some((_storage, _socket, _server, mut client, sandbox_id, mut sandbox_config)) =
        setup().await
    else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };

    let log_dir = tempfile::tempdir().unwrap();
    sandbox_config.log_directory = log_dir.path().display().to_string();

    let mut config = container_config("rotator", 0);
    config.command = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "while true; do echo tick; sleep 1; done".to_string(),
    ];
    config.log_path = "rotator.log".to_string();
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

    // Reopen before start: cri-o's own verbatim error.
    let err = client
        .reopen_container_log(oci_cri_types::ReopenContainerLogRequest {
            container_id: container_id.clone(),
        })
        .await
        .expect_err("reopen of a never-started container should fail");
    assert_eq!(err.code(), tonic::Code::FailedPrecondition);
    assert!(
        err.message().contains("container is not running"),
        "{err:?}"
    );

    client
        .start_container(oci_cri_types::StartContainerRequest {
            container_id: container_id.clone(),
        })
        .await
        .unwrap();
    wait_for_state(&mut client, &container_id, ContainerState::ContainerRunning).await;

    // Wait for the first tick, then rotate: rename the file away and
    // tell the logger to reopen.
    let log_path = log_dir.path().join("rotator.log");
    let rotated_path = log_dir.path().join("rotator.log.1");
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while std::fs::read_to_string(&log_path)
        .unwrap_or_default()
        .is_empty()
    {
        assert!(std::time::Instant::now() < deadline, "no first tick");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    std::fs::rename(&log_path, &rotated_path).unwrap();
    client
        .reopen_container_log(oci_cri_types::ReopenContainerLogRequest {
            container_id: container_id.clone(),
        })
        .await
        .expect("ReopenContainerLog failed");

    // New lines land in a fresh file at the original path.
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        let contents = std::fs::read_to_string(&log_path).unwrap_or_default();
        if contents.contains(" stdout F tick") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the reopened log never received a tick; new: {contents:?}, rotated: {:?}",
            std::fs::read_to_string(&rotated_path).unwrap_or_default()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // The rotated file kept the pre-rotation lines.
    assert!(
        std::fs::read_to_string(&rotated_path)
            .unwrap()
            .contains(" stdout F tick"),
        "rotated file should retain old lines"
    );

    // A running container with no log path has no logger to reopen.
    let mut config = container_config("no-log-rotator", 0);
    config.command = vec!["/bin/sleep".to_string(), "300".to_string()];
    let no_log_id = client
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
        .start_container(oci_cri_types::StartContainerRequest {
            container_id: no_log_id.clone(),
        })
        .await
        .unwrap();
    wait_for_state(&mut client, &no_log_id, ContainerState::ContainerRunning).await;
    let err = client
        .reopen_container_log(oci_cri_types::ReopenContainerLogRequest {
            container_id: no_log_id.clone(),
        })
        .await
        .expect_err("reopen without a log path should fail");
    assert_eq!(err.code(), tonic::Code::FailedPrecondition);
    assert!(err.message().contains("no log path"), "{err:?}");

    // Unknown container: NotFound. Then clean up both runners.
    let err = client
        .reopen_container_log(oci_cri_types::ReopenContainerLogRequest {
            container_id: "deadbeef".repeat(8),
        })
        .await
        .expect_err("reopen of an unknown container should fail");
    assert_eq!(err.code(), tonic::Code::NotFound);
    for id in [container_id, no_log_id] {
        client
            .stop_container(oci_cri_types::StopContainerRequest {
                container_id: id,
                timeout: 0,
            })
            .await
            .unwrap();
    }
}

/// `StopContainer`'s graceful phase sends the image's own declared
/// `STOPSIGNAL` (`docs/design/0244`) instead of `SIGTERM` — the
/// container's USR1 trap exit code (43) proves which signal actually
/// arrived, exactly like real cri-o's own `GetStopSignal` behavior.
#[tokio::test]
async fn stop_container_honors_the_images_stopsignal() {
    let Some((storage, _socket, _server, mut client, sandbox_id, sandbox_config)) = setup().await
    else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };

    let stopsignal_image = "docker.io/ocicri-test/stopsignal:latest";
    let busybox = busybox_path().unwrap();
    let store = Store::open(storage.path()).unwrap();
    seed_image(
        &store,
        stopsignal_image,
        &busybox,
        &["sh", "sleep"],
        ContainerConfig {
            stop_signal: Some("SIGUSR1".to_string()),
            ..Default::default()
        },
    );

    let mut config = container_config("usr1-stopper", 0);
    config.image = Some(ImageSpec {
        image: stopsignal_image.to_string(),
        ..Default::default()
    });
    // Distinct exit codes per signal; `touch /ready` after the traps
    // (the same pre-exec-race guard 0238's own stop test documents).
    config.command = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "trap 'exit 43' USR1; trap 'exit 21' TERM; touch /ready; while true; do sleep 0.2; done"
            .to_string(),
    ];
    let container_id = client
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
        .start_container(oci_cri_types::StartContainerRequest {
            container_id: container_id.clone(),
        })
        .await
        .unwrap();
    wait_for_state(&mut client, &container_id, ContainerState::ContainerRunning).await;
    let ready = bundle_dir(storage.path(), &container_id).join("rootfs/ready");
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while !ready.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "traps never installed"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    client
        .stop_container(oci_cri_types::StopContainerRequest {
            container_id: container_id.clone(),
            timeout: 30,
        })
        .await
        .expect("StopContainer failed");
    let status = wait_for_state(&mut client, &container_id, ContainerState::ContainerExited).await;
    assert_eq!(
        status.exit_code, 43,
        "the USR1 trap's own exit code proves STOPSIGNAL was sent: {status:?}"
    );
}

/// `ContainerConfig.mounts` (0304): a plain bind mount translates
/// into a real OCI spec `Mount` entry -- matching real cri-o's own
/// `["rbind", "rprivate"]` option pair for the private-propagation
/// default, plus `"ro"` when `readonly` is set. Also covers real
/// cri-o's own checked-directly "auto-create a missing host path"
/// behavior (`~/git/cri-o/server/container_create_linux.go`'s own
/// `addOCIBindMounts`: `os.MkdirAll`, not an error, despite the
/// proto's own stricter-sounding doc comment).
#[tokio::test]
async fn create_container_translates_a_plain_bind_mount_into_the_generated_spec() {
    let Some((storage, _socket, _server, mut client, sandbox_id, sandbox_config)) = setup().await
    else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };

    let host_dir = tempfile::tempdir().unwrap();
    // A real subdirectory that doesn't exist yet -- proves the
    // missing-host-path auto-mkdir behavior, not just the mount-
    // translation shape.
    let missing_host_path = host_dir.path().join("not-yet-created");

    let mut config = container_config("bind-mount-test", 0);
    config.mounts = vec![
        Mount {
            container_path: "/data".to_string(),
            host_path: missing_host_path.display().to_string(),
            readonly: false,
            ..Default::default()
        },
        Mount {
            container_path: "/readonly-data".to_string(),
            host_path: host_dir.path().display().to_string(),
            readonly: true,
            ..Default::default()
        },
    ];

    let container_id = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id,
            config: Some(config),
            sandbox_config: Some(sandbox_config),
        })
        .await
        .expect("CreateContainer failed")
        .into_inner()
        .container_id;

    assert!(
        missing_host_path.is_dir(),
        "a missing host_path should be auto-created as a real directory, matching real cri-o"
    );

    let spec: serde_json::Value = serde_json::from_slice(
        &std::fs::read(bundle_dir(storage.path(), &container_id).join("config.json")).unwrap(),
    )
    .unwrap();
    let mounts = spec["mounts"].as_array().unwrap();
    let rw_mount = mounts
        .iter()
        .find(|m| m["destination"] == "/data")
        .expect("the read-write bind mount should be present");
    assert_eq!(rw_mount["type"], "bind");
    assert_eq!(rw_mount["source"], missing_host_path.display().to_string());
    let rw_options: Vec<&str> = rw_mount["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(rw_options.contains(&"rbind"), "{rw_options:?}");
    assert!(rw_options.contains(&"rprivate"), "{rw_options:?}");
    assert!(!rw_options.contains(&"ro"), "{rw_options:?}");

    let ro_mount = mounts
        .iter()
        .find(|m| m["destination"] == "/readonly-data")
        .expect("the read-only bind mount should be present");
    let ro_options: Vec<&str> = ro_mount["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(ro_options.contains(&"ro"), "{ro_options:?}");
}

/// Every deliberately-out-of-scope `Mount` field (0304's own doc
/// comment) is a real, honest `Status::unimplemented` rather than a
/// silent misinterpretation -- matching this project's own
/// established convention for every other "narrow first slice"
/// feature. Empty `ContainerPath`/`HostPath` are real client-input
/// errors instead, matching real cri-o's own exact error strings.
#[tokio::test]
async fn create_container_rejects_unsupported_mount_fields_clearly() {
    let Some((_storage, _socket, _server, mut client, sandbox_id, sandbox_config)) = setup().await
    else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };

    async fn expect_status(
        client: &mut RuntimeServiceClient<tonic::transport::Channel>,
        sandbox_id: &str,
        sandbox_config: &PodSandboxConfig,
        name: &str,
        mount: Mount,
    ) -> tonic::Status {
        let mut config = container_config(name, 0);
        config.mounts = vec![mount];
        client
            .create_container(CreateContainerRequest {
                pod_sandbox_id: sandbox_id.to_string(),
                config: Some(config),
                sandbox_config: Some(sandbox_config.clone()),
            })
            .await
            .expect_err("should have been rejected")
    }

    let empty_container_path = expect_status(
        &mut client,
        &sandbox_id,
        &sandbox_config,
        "empty-container-path",
        Mount {
            host_path: "/tmp".to_string(),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(empty_container_path.code(), tonic::Code::InvalidArgument);
    assert!(
        empty_container_path
            .message()
            .contains("mount.ContainerPath is empty"),
        "{empty_container_path:?}"
    );

    let empty_host_path = expect_status(
        &mut client,
        &sandbox_id,
        &sandbox_config,
        "empty-host-path",
        Mount {
            container_path: "/data".to_string(),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(empty_host_path.code(), tonic::Code::InvalidArgument);
    assert!(
        empty_host_path
            .message()
            .contains("mount.HostPath is empty"),
        "{empty_host_path:?}"
    );

    let image_mount = expect_status(
        &mut client,
        &sandbox_id,
        &sandbox_config,
        "image-mount",
        Mount {
            container_path: "/data".to_string(),
            image: Some(ImageSpec {
                image: IMAGE.to_string(),
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(image_mount.code(), tonic::Code::Unimplemented);

    let selinux = expect_status(
        &mut client,
        &sandbox_id,
        &sandbox_config,
        "selinux-mount",
        Mount {
            container_path: "/data".to_string(),
            host_path: "/tmp".to_string(),
            selinux_relabel: true,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(selinux.code(), tonic::Code::Unimplemented);

    let propagation = expect_status(
        &mut client,
        &sandbox_id,
        &sandbox_config,
        "propagation-mount",
        Mount {
            container_path: "/data".to_string(),
            host_path: "/tmp".to_string(),
            propagation: MountPropagation::PropagationHostToContainer as i32,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(propagation.code(), tonic::Code::Unimplemented);

    let recursive_ro = expect_status(
        &mut client,
        &sandbox_id,
        &sandbox_config,
        "recursive-ro-mount",
        Mount {
            container_path: "/data".to_string(),
            host_path: "/tmp".to_string(),
            readonly: true,
            recursive_read_only: true,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(recursive_ro.code(), tonic::Code::Unimplemented);

    let id_mapped = expect_status(
        &mut client,
        &sandbox_id,
        &sandbox_config,
        "id-mapped-mount",
        Mount {
            container_path: "/data".to_string(),
            host_path: "/tmp".to_string(),
            uid_mappings: vec![IdMapping {
                host_id: 0,
                container_id: 0,
                length: 1,
            }],
            ..Default::default()
        },
    )
    .await;
    assert_eq!(id_mapped.code(), tonic::Code::Unimplemented);
}

/// `ContainerConfig.linux.security_context`'s own `run_as_user`/
/// `run_as_group`/`run_as_username` (`docs/design/0365`): a real,
/// explicit `run_as_user: 0`/`run_as_group: 0` request (a legitimate
/// case many real pods make explicitly) already matches this
/// project's own existing default and must succeed exactly like a
/// request with no security context at all.
#[tokio::test]
async fn create_container_run_as_user_and_group_zero_succeeds() {
    let Some((_storage, _socket, _server, mut client, sandbox_id, sandbox_config)) = setup().await
    else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };

    let mut config = container_config("run-as-root", 0);
    config.linux = Some(oci_cri_types::LinuxContainerConfig {
        security_context: Some(oci_cri_types::LinuxContainerSecurityContext {
            run_as_user: Some(oci_cri_types::Int64Value { value: 0 }),
            run_as_group: Some(oci_cri_types::Int64Value { value: 0 }),
            ..Default::default()
        }),
        ..Default::default()
    });
    let created = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id.clone(),
            config: Some(config),
            sandbox_config: Some(sandbox_config.clone()),
        })
        .await;
    assert!(created.is_ok(), "{created:?}");
}

/// `security_context.readonly_rootfs` (`docs/design/0388`): a real,
/// explicit `readonly_rootfs: true` request must set `root.readonly`
/// in the real generated `config.json` -- previously silently ignored
/// (`build_spec` used to force `readonly = false` unconditionally
/// regardless of the request), the same shape of bug `0365` already
/// fixed for `run_as_user`. Checked the same host-independent way
/// `ociman_run.rs`'s own `run_read_only_sets_root_readonly_in_the_
/// real_spec` checks its own identical `--read-only` flag (reading
/// the actual spec back out), **not** by asserting a real in-container
/// write attempt fails: that test's own doc comment already found,
/// the hard way, that remounting `/` read-only can silently no-op
/// under this project's own rootless "fake root in a userns" model on
/// some hosts (`oci_runtime_core::launch`'s own `RemountReadonly`
/// handler tolerates the exact same real `CAP_SYS_ADMIN`-in-the-
/// owning-namespace limitation for `/sys`), so a real write-attempt
/// assertion here would be exactly as host-dependent, not a stronger
/// check.
#[tokio::test]
async fn create_container_readonly_rootfs_sets_root_readonly_in_the_real_spec() {
    let Some((storage, _socket, _server, mut client, sandbox_id, sandbox_config)) = setup().await
    else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };

    let mut readonly_config = container_config("readonly-rootfs-test", 0);
    readonly_config.linux = Some(oci_cri_types::LinuxContainerConfig {
        security_context: Some(oci_cri_types::LinuxContainerSecurityContext {
            readonly_rootfs: true,
            ..Default::default()
        }),
        ..Default::default()
    });
    let readonly_id = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id.clone(),
            config: Some(readonly_config),
            sandbox_config: Some(sandbox_config.clone()),
        })
        .await
        .expect("CreateContainer failed")
        .into_inner()
        .container_id;

    let spec: serde_json::Value = serde_json::from_slice(
        &std::fs::read(bundle_dir(storage.path(), &readonly_id).join("config.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        spec["root"]["readonly"],
        serde_json::json!(true),
        "expected readonly_rootfs: true to set root.readonly: {spec:?}"
    );

    // Contrast: an otherwise-identical container with no security
    // context at all (the common, unconfigured default) keeps the
    // existing writable-by-default behavior unchanged -- a real
    // regression guard for the exact bug this closes (`build_spec`
    // used to force `readonly = false` unconditionally).
    let writable_config = container_config("writable-rootfs-test", 0);
    let writable_id = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id.clone(),
            config: Some(writable_config),
            sandbox_config: Some(sandbox_config.clone()),
        })
        .await
        .expect("CreateContainer failed")
        .into_inner()
        .container_id;
    let writable_spec: serde_json::Value = serde_json::from_slice(
        &std::fs::read(bundle_dir(storage.path(), &writable_id).join("config.json")).unwrap(),
    )
    .unwrap();
    assert_ne!(
        writable_spec["root"]["readonly"],
        serde_json::json!(true),
        "the unconfigured default must stay writable: {writable_spec:?}"
    );
}

/// `security_context.masked_paths` (`docs/design/0391`): a real,
/// explicit extra masked path must genuinely be masked (bind-mounted
/// over with `/dev/null`) inside a real started container -- proven
/// end to end, unlike `readonly_rootfs`'s own spec-only check, since
/// masking a real, already-existing file is a plain, fresh bind mount
/// entirely within this project's own unprivileged user namespace's
/// authority (no `CAP_SYS_ADMIN`-in-the-owning-namespace remount
/// concern the read-only cases are subject to). `/etc/hosts` (0296)
/// is a real file this project's own `CreateContainer` already writes
/// into the extracted rootfs before the container ever starts, so
/// it's masked by the time the container's own mount namespace is set
/// up.
#[tokio::test]
async fn create_container_masked_paths_genuinely_masks_a_real_file_inside_the_running_container() {
    let Some((_storage, _socket, _server, mut client, sandbox_id, sandbox_config)) = setup().await
    else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };

    let mut config = container_config("masked-paths-test", 0);
    config.command = vec!["/bin/sleep".to_string(), "300".to_string()];
    config.linux = Some(oci_cri_types::LinuxContainerConfig {
        security_context: Some(oci_cri_types::LinuxContainerSecurityContext {
            masked_paths: vec!["/etc/hosts".to_string()],
            ..Default::default()
        }),
        ..Default::default()
    });
    let container_id = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id.clone(),
            config: Some(config),
            sandbox_config: Some(sandbox_config),
        })
        .await
        .expect("CreateContainer failed")
        .into_inner()
        .container_id;
    client
        .start_container(oci_cri_types::StartContainerRequest {
            container_id: container_id.clone(),
        })
        .await
        .unwrap();
    wait_for_state(&mut client, &container_id, ContainerState::ContainerRunning).await;

    let response = client
        .exec_sync(oci_cri_types::ExecSyncRequest {
            container_id: container_id.clone(),
            cmd: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "cat /etc/hosts | wc -c".to_string(),
            ],
            timeout: 0,
        })
        .await
        .expect("ExecSync failed")
        .into_inner();
    assert_eq!(response.exit_code, 0, "{response:?}");
    assert_eq!(
        String::from_utf8_lossy(&response.stdout).trim(),
        "0",
        "a masked /etc/hosts should read back as a real, empty /dev/null"
    );

    client
        .stop_container(oci_cri_types::StopContainerRequest {
            container_id,
            timeout: 0,
        })
        .await
        .unwrap();
}

/// `security_context.capabilities.add_capabilities`/`.drop_
/// capabilities` (`docs/design/0392`): previously every CRI container
/// got exactly the same hardcoded real `podman`-default 11-capability
/// set, no matter what a pod's own `capabilities` actually requested.
/// Verified end to end via a real `/proc/self/status` read inside a
/// running container -- the same real bitmask-diffing technique
/// `ocirun_exec.rs`'s own `exec_cap_adds_a_capability_on_top_of_the_
/// containers_own_default_set` test already established, ported here
/// for `ocicri`'s own real `podman`-default base (11 capabilities,
/// not `ocirun`'s own smaller 3-capability `Spec::example()` default)
/// -- computed programmatically (not hand-derived) from `oci_spec_
/// types::runtime::podman_default_capabilities()`'s own documented bit
/// positions to avoid a transcription error: `CAP_CHOWN`(0)|`CAP_DAC_
/// OVERRIDE`(1)|`CAP_FOWNER`(3)|`CAP_FSETID`(4)|`CAP_KILL`(5)|`CAP_
/// NET_BIND_SERVICE`(10)|`CAP_SETFCAP`(31)|`CAP_SETGID`(6)|`CAP_
/// SETPCAP`(8)|`CAP_SETUID`(7)|`CAP_SYS_CHROOT`(18) = `0x800405fb`.
#[tokio::test]
async fn create_container_capabilities_add_and_drop_change_the_real_process_capability_sets() {
    let Some((_storage, _socket, _server, mut client, sandbox_id, sandbox_config)) = setup().await
    else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };

    let grep_caps = r#"grep -E "^(CapPrm|CapEff|CapBnd|CapAmb):" /proc/self/status"#;

    async fn run_and_get_caps(
        client: &mut RuntimeServiceClient<tonic::transport::Channel>,
        sandbox_id: &str,
        sandbox_config: &PodSandboxConfig,
        name: &str,
        security_context: Option<oci_cri_types::LinuxContainerSecurityContext>,
        grep_caps: &str,
    ) -> String {
        let mut config = container_config(name, 0);
        config.command = vec!["/bin/sleep".to_string(), "300".to_string()];
        if let Some(security_context) = security_context {
            config.linux = Some(oci_cri_types::LinuxContainerConfig {
                security_context: Some(security_context),
                ..Default::default()
            });
        }
        let container_id = client
            .create_container(CreateContainerRequest {
                pod_sandbox_id: sandbox_id.to_string(),
                config: Some(config),
                sandbox_config: Some(sandbox_config.clone()),
            })
            .await
            .expect("CreateContainer failed")
            .into_inner()
            .container_id;
        client
            .start_container(oci_cri_types::StartContainerRequest {
                container_id: container_id.clone(),
            })
            .await
            .unwrap();
        wait_for_state(client, &container_id, ContainerState::ContainerRunning).await;
        let response = client
            .exec_sync(oci_cri_types::ExecSyncRequest {
                container_id: container_id.clone(),
                cmd: vec![
                    "/bin/sh".to_string(),
                    "-c".to_string(),
                    grep_caps.to_string(),
                ],
                timeout: 0,
            })
            .await
            .expect("ExecSync failed")
            .into_inner();
        assert_eq!(response.exit_code, 0, "{response:?}");
        client
            .stop_container(oci_cri_types::StopContainerRequest {
                container_id,
                timeout: 0,
            })
            .await
            .unwrap();
        String::from_utf8_lossy(&response.stdout).trim().to_string()
    }

    let default_caps = run_and_get_caps(
        &mut client,
        &sandbox_id,
        &sandbox_config,
        "caps-default",
        None,
        grep_caps,
    )
    .await;
    assert_eq!(
        default_caps,
        "CapPrm:\t00000000800405fb\nCapEff:\t00000000800405fb\nCapBnd:\t00000000800405fb\nCapAmb:\t0000000000000000",
        "the unconfigured default should be the real podman-default 11-capability set: {default_caps:?}"
    );

    let with_add = run_and_get_caps(
        &mut client,
        &sandbox_id,
        &sandbox_config,
        "caps-add",
        Some(oci_cri_types::LinuxContainerSecurityContext {
            capabilities: Some(oci_cri_types::Capability {
                add_capabilities: vec!["NET_ADMIN".to_string()],
                ..Default::default()
            }),
            ..Default::default()
        }),
        grep_caps,
    )
    .await;
    assert_eq!(
        with_add,
        "CapPrm:\t00000000800415fb\nCapEff:\t00000000800415fb\nCapBnd:\t00000000800415fb\nCapAmb:\t0000000000000000",
        "add_capabilities: [NET_ADMIN] should add exactly bit 12 on top of the default set: {with_add:?}"
    );

    let with_drop = run_and_get_caps(
        &mut client,
        &sandbox_id,
        &sandbox_config,
        "caps-drop",
        Some(oci_cri_types::LinuxContainerSecurityContext {
            capabilities: Some(oci_cri_types::Capability {
                drop_capabilities: vec!["CHOWN".to_string()],
                ..Default::default()
            }),
            ..Default::default()
        }),
        grep_caps,
    )
    .await;
    assert_eq!(
        with_drop,
        "CapPrm:\t00000000800405fa\nCapEff:\t00000000800405fa\nCapBnd:\t00000000800405fa\nCapAmb:\t0000000000000000",
        "drop_capabilities: [CHOWN] should clear exactly bit 0 from the default set: {with_drop:?}"
    );
}

/// A non-root `run_as_user`/`run_as_group`, `run_as_username` at all,
/// and `run_as_group` given without `run_as_user`/`run_as_username`
/// are each a real, clear error — see `validate_run_as_user`'s own
/// doc comment (in `runtime_service.rs`) for exactly why each one is.
#[tokio::test]
async fn create_container_rejects_unsupported_run_as_user_fields_clearly() {
    let Some((_storage, _socket, _server, mut client, sandbox_id, sandbox_config)) = setup().await
    else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };

    async fn expect_status(
        client: &mut RuntimeServiceClient<tonic::transport::Channel>,
        sandbox_id: &str,
        sandbox_config: &PodSandboxConfig,
        name: &str,
        security_context: oci_cri_types::LinuxContainerSecurityContext,
    ) -> tonic::Status {
        let mut config = container_config(name, 0);
        config.linux = Some(oci_cri_types::LinuxContainerConfig {
            security_context: Some(security_context),
            ..Default::default()
        });
        client
            .create_container(CreateContainerRequest {
                pod_sandbox_id: sandbox_id.to_string(),
                config: Some(config),
                sandbox_config: Some(sandbox_config.clone()),
            })
            .await
            .expect_err("should have been rejected")
    }

    let nonzero_user = expect_status(
        &mut client,
        &sandbox_id,
        &sandbox_config,
        "run-as-user-1000",
        oci_cri_types::LinuxContainerSecurityContext {
            run_as_user: Some(oci_cri_types::Int64Value { value: 1000 }),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(nonzero_user.code(), tonic::Code::Unimplemented);
    assert!(
        nonzero_user.message().contains("run_as_user 1000"),
        "{nonzero_user:?}"
    );

    let nonzero_group = expect_status(
        &mut client,
        &sandbox_id,
        &sandbox_config,
        "run-as-group-1000",
        oci_cri_types::LinuxContainerSecurityContext {
            run_as_user: Some(oci_cri_types::Int64Value { value: 0 }),
            run_as_group: Some(oci_cri_types::Int64Value { value: 1000 }),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(nonzero_group.code(), tonic::Code::Unimplemented);
    assert!(
        nonzero_group.message().contains("run_as_group 1000"),
        "{nonzero_group:?}"
    );

    let username = expect_status(
        &mut client,
        &sandbox_id,
        &sandbox_config,
        "run-as-username",
        oci_cri_types::LinuxContainerSecurityContext {
            run_as_username: "nobody".to_string(),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(username.code(), tonic::Code::Unimplemented);
    assert!(
        username.message().contains("run_as_username"),
        "{username:?}"
    );

    let group_without_user = expect_status(
        &mut client,
        &sandbox_id,
        &sandbox_config,
        "group-without-user",
        oci_cri_types::LinuxContainerSecurityContext {
            run_as_group: Some(oci_cri_types::Int64Value { value: 0 }),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(group_without_user.code(), tonic::Code::InvalidArgument);
    assert!(
        group_without_user
            .message()
            .contains("user group is specified without user or username"),
        "{group_without_user:?}"
    );
}

/// `security_context.privileged` (`docs/design/0389`): a real,
/// explicit `privileged: true` request must be a clear, honest
/// `Status::unimplemented` -- previously silently ignored entirely
/// (not read anywhere at all), so a workload asking for privileged
/// access used to get an ordinary, confined container instead with no
/// error telling it so. `privileged: false` (the common, unconfigured
/// default) must still succeed exactly like a request with no
/// security context at all.
#[tokio::test]
async fn create_container_rejects_privileged_clearly_but_allows_the_default() {
    let Some((_storage, _socket, _server, mut client, sandbox_id, sandbox_config)) = setup().await
    else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };

    let mut privileged_config = container_config("privileged-test", 0);
    privileged_config.linux = Some(oci_cri_types::LinuxContainerConfig {
        security_context: Some(oci_cri_types::LinuxContainerSecurityContext {
            privileged: true,
            ..Default::default()
        }),
        ..Default::default()
    });
    let rejected = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id.clone(),
            config: Some(privileged_config),
            sandbox_config: Some(sandbox_config.clone()),
        })
        .await
        .expect_err("privileged: true should be rejected");
    assert_eq!(rejected.code(), tonic::Code::Unimplemented);
    assert!(
        rejected
            .message()
            .contains("privileged containers are not yet supported"),
        "{rejected:?}"
    );

    let mut unprivileged_config = container_config("unprivileged-test", 0);
    unprivileged_config.linux = Some(oci_cri_types::LinuxContainerConfig {
        security_context: Some(oci_cri_types::LinuxContainerSecurityContext {
            privileged: false,
            ..Default::default()
        }),
        ..Default::default()
    });
    let allowed = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id.clone(),
            config: Some(unprivileged_config),
            sandbox_config: Some(sandbox_config.clone()),
        })
        .await;
    assert!(allowed.is_ok(), "{allowed:?}");
}

/// `security_context.supplemental_groups` (`docs/design/0399`): a
/// real, explicit non-zero entry must be a clear, honest
/// `Status::unimplemented` naming the offending value -- previously
/// silently ignored entirely (not read anywhere at all), so a pod's
/// own explicit `securityContext.supplementalGroups: [1000]` request
/// used to be dropped with no error telling it so, the same shape of
/// bug `0365` already fixed for `run_as_user`/`run_as_group`. An
/// empty list, or one containing only `0` (already this project's own
/// existing default), must still succeed exactly like a request with
/// no security context at all.
#[tokio::test]
async fn create_container_rejects_a_non_zero_supplemental_group_but_allows_the_default() {
    let Some((_storage, _socket, _server, mut client, sandbox_id, sandbox_config)) = setup().await
    else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };

    let mut rejected_config = container_config("supplemental-group-test", 0);
    rejected_config.linux = Some(oci_cri_types::LinuxContainerConfig {
        security_context: Some(oci_cri_types::LinuxContainerSecurityContext {
            supplemental_groups: vec![1000],
            ..Default::default()
        }),
        ..Default::default()
    });
    let rejected = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id.clone(),
            config: Some(rejected_config),
            sandbox_config: Some(sandbox_config.clone()),
        })
        .await
        .expect_err("a non-zero supplemental group should be rejected");
    assert_eq!(rejected.code(), tonic::Code::Unimplemented);
    assert!(
        rejected.message().contains("supplemental_groups 1000"),
        "{rejected:?}"
    );

    let mut allowed_config = container_config("supplemental-group-zero-test", 0);
    allowed_config.linux = Some(oci_cri_types::LinuxContainerConfig {
        security_context: Some(oci_cri_types::LinuxContainerSecurityContext {
            supplemental_groups: vec![0],
            ..Default::default()
        }),
        ..Default::default()
    });
    let allowed = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id.clone(),
            config: Some(allowed_config),
            sandbox_config: Some(sandbox_config.clone()),
        })
        .await;
    assert!(allowed.is_ok(), "{allowed:?}");
}

/// End-to-end proof the bind mount is genuinely live at runtime, not
/// just declared in `config.json`: a real file written on the host
/// side of the mount is readable from *inside* the running container
/// via a real `ExecSync`, and a file the container itself writes
/// through the mount is visible back on the host side too.
#[tokio::test]
async fn create_container_bind_mount_is_genuinely_live_at_runtime() {
    let Some((_storage, _socket, _server, mut client, sandbox_id, sandbox_config)) = setup().await
    else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };

    let host_dir = tempfile::tempdir().unwrap();
    std::fs::write(host_dir.path().join("from-host.txt"), b"hello from host").unwrap();

    let mut config = container_config("live-mount-test", 0);
    config.command = vec!["/bin/sleep".to_string(), "300".to_string()];
    config.mounts = vec![Mount {
        container_path: "/mnt/shared".to_string(),
        host_path: host_dir.path().display().to_string(),
        readonly: false,
        ..Default::default()
    }];

    let container_id = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id,
            config: Some(config),
            sandbox_config: Some(sandbox_config),
        })
        .await
        .expect("CreateContainer failed")
        .into_inner()
        .container_id;

    client
        .start_container(oci_cri_types::StartContainerRequest {
            container_id: container_id.clone(),
        })
        .await
        .expect("StartContainer failed");
    wait_for_state(&mut client, &container_id, ContainerState::ContainerRunning).await;

    let read_back = client
        .exec_sync(oci_cri_types::ExecSyncRequest {
            container_id: container_id.clone(),
            cmd: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "cat /mnt/shared/from-host.txt".to_string(),
            ],
            timeout: 0,
        })
        .await
        .expect("ExecSync failed")
        .into_inner();
    assert_eq!(read_back.exit_code, 0, "{read_back:?}");
    assert_eq!(
        String::from_utf8_lossy(&read_back.stdout),
        "hello from host"
    );

    let write_from_container = client
        .exec_sync(oci_cri_types::ExecSyncRequest {
            container_id: container_id.clone(),
            cmd: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "echo hello from container > /mnt/shared/from-container.txt".to_string(),
            ],
            timeout: 0,
        })
        .await
        .expect("ExecSync failed")
        .into_inner();
    assert_eq!(
        write_from_container.exit_code, 0,
        "{write_from_container:?}"
    );
    assert_eq!(
        std::fs::read_to_string(host_dir.path().join("from-container.txt")).unwrap(),
        "hello from container\n"
    );
}

/// A real, previously-shipped bug (0305): `build_cri_bind_mounts` used
/// to call `fs::create_dir_all` on *every* `host_path`
/// unconditionally, which fails outright (`EEXIST`) when `host_path`
/// is already an existing single file -- a common real kubelet case
/// (`/etc/localtime`, a ConfigMap key, `/etc/machine-id`, ...). Fixed
/// to match real cri-o's own exact `resolveSymbolicLink` +
/// conditional-`os.MkdirAll` logic: an already-existing path (file or
/// directory) is used exactly as given, never touched.
#[tokio::test]
async fn create_container_bind_mounts_an_already_existing_single_file() {
    let Some((_storage, _socket, _server, mut client, sandbox_id, sandbox_config)) = setup().await
    else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };

    let host_dir = tempfile::tempdir().unwrap();
    let host_file = host_dir.path().join("single-file.txt");
    std::fs::write(&host_file, b"a real single file, not a directory").unwrap();

    let mut config = container_config("single-file-mount-test", 0);
    config.command = vec!["/bin/sleep".to_string(), "300".to_string()];
    config.mounts = vec![Mount {
        container_path: "/etc/injected-file".to_string(),
        host_path: host_file.display().to_string(),
        readonly: true,
        ..Default::default()
    }];

    let container_id = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id,
            config: Some(config),
            sandbox_config: Some(sandbox_config),
        })
        .await
        .expect("CreateContainer failed -- a single-file host_path should be usable as-is")
        .into_inner()
        .container_id;

    // The host's own file must be completely untouched (still a real
    // file, still its own original content) -- proving it was never
    // routed through `create_dir_all` at all.
    assert!(host_file.is_file());
    assert_eq!(
        std::fs::read_to_string(&host_file).unwrap(),
        "a real single file, not a directory"
    );

    client
        .start_container(oci_cri_types::StartContainerRequest {
            container_id: container_id.clone(),
        })
        .await
        .expect("StartContainer failed");
    wait_for_state(&mut client, &container_id, ContainerState::ContainerRunning).await;

    let read_back = client
        .exec_sync(oci_cri_types::ExecSyncRequest {
            container_id: container_id.clone(),
            cmd: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "cat /etc/injected-file".to_string(),
            ],
            timeout: 0,
        })
        .await
        .expect("ExecSync failed")
        .into_inner();
    assert_eq!(read_back.exit_code, 0, "{read_back:?}");
    assert_eq!(
        String::from_utf8_lossy(&read_back.stdout),
        "a real single file, not a directory"
    );
}

/// A symlinked `host_path` is followed to its real target (0305),
/// matching real cri-o's own `resolveSymbolicLink` exactly -- checked
/// via the mounted content actually being the symlink's own real
/// target's content, not a failure or an empty/broken mount.
#[tokio::test]
async fn create_container_bind_mount_follows_a_symlinked_host_path() {
    let Some((_storage, _socket, _server, mut client, sandbox_id, sandbox_config)) = setup().await
    else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };

    let host_dir = tempfile::tempdir().unwrap();
    let real_target = host_dir.path().join("real-target.txt");
    std::fs::write(&real_target, b"the real symlink target content").unwrap();
    let symlink_path = host_dir.path().join("a-symlink.txt");
    std::os::unix::fs::symlink(&real_target, &symlink_path).unwrap();

    let mut config = container_config("symlink-mount-test", 0);
    config.command = vec!["/bin/sleep".to_string(), "300".to_string()];
    config.mounts = vec![Mount {
        container_path: "/etc/via-symlink".to_string(),
        host_path: symlink_path.display().to_string(),
        readonly: true,
        ..Default::default()
    }];

    let container_id = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id,
            config: Some(config),
            sandbox_config: Some(sandbox_config),
        })
        .await
        .expect("CreateContainer failed")
        .into_inner()
        .container_id;

    client
        .start_container(oci_cri_types::StartContainerRequest {
            container_id: container_id.clone(),
        })
        .await
        .expect("StartContainer failed");
    wait_for_state(&mut client, &container_id, ContainerState::ContainerRunning).await;

    let read_back = client
        .exec_sync(oci_cri_types::ExecSyncRequest {
            container_id: container_id.clone(),
            cmd: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "cat /etc/via-symlink".to_string(),
            ],
            timeout: 0,
        })
        .await
        .expect("ExecSync failed")
        .into_inner();
    assert_eq!(read_back.exit_code, 0, "{read_back:?}");
    assert_eq!(
        String::from_utf8_lossy(&read_back.stdout),
        "the real symlink target content"
    );
}

/// `ImageService::RemoveImage` refuses to remove an image a real,
/// persisted CRI container record still references -- matching real
/// cri-o's own `volumeInUse` check (`~/git/cri-o/server/
/// image_remove.go`) and real `container-libs/storage`'s own
/// identical `DeleteImage` rule: any container state counts, not just
/// running ones (this test uses a merely `Created`, never-started
/// container, the narrowest possible case). Removing the container
/// first, then the image, succeeds -- a regression guard against the
/// new check being over-broad.
#[tokio::test]
async fn remove_image_refuses_while_a_container_still_references_it() {
    let Some((_storage, socket, _server, mut client, sandbox_id, sandbox_config)) = setup().await
    else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };

    let container_id = client
        .create_container(CreateContainerRequest {
            pod_sandbox_id: sandbox_id,
            config: Some(container_config("image-in-use", 0)),
            sandbox_config: Some(sandbox_config),
        })
        .await
        .expect("CreateContainer failed")
        .into_inner()
        .container_id;

    let socket_path = socket.path().join("ocicri.sock");
    let mut images = connect_image_service(socket_path).await;

    let refused = images
        .remove_image(RemoveImageRequest {
            image: Some(ImageSpec {
                image: IMAGE.to_string(),
                ..Default::default()
            }),
        })
        .await
        .expect_err("RemoveImage must refuse while a container still references this image");
    assert_eq!(
        refused.code(),
        tonic::Code::Unknown,
        "matching real cri-o's own bare, unwrapped Go error for this exact case: {refused:?}"
    );
    assert!(
        refused.message().contains(&container_id),
        "the error should name the real, dependent container id: {refused:?}"
    );

    // Still fully present afterward -- the refused removal must not
    // have partially deleted anything.
    let still_present = images
        .image_status(oci_cri_types::ImageStatusRequest {
            image: Some(ImageSpec {
                image: IMAGE.to_string(),
                ..Default::default()
            }),
            verbose: false,
        })
        .await
        .expect("ImageStatus failed")
        .into_inner();
    assert!(still_present.image.is_some(), "{still_present:?}");

    // Remove the container, then the same removal succeeds.
    client
        .remove_container(RemoveContainerRequest {
            container_id: container_id.clone(),
        })
        .await
        .expect("RemoveContainer failed");

    images
        .remove_image(RemoveImageRequest {
            image: Some(ImageSpec {
                image: IMAGE.to_string(),
                ..Default::default()
            }),
        })
        .await
        .expect("RemoveImage should succeed once no container references the image anymore");

    let gone = images
        .image_status(oci_cri_types::ImageStatusRequest {
            image: Some(ImageSpec {
                image: IMAGE.to_string(),
                ..Default::default()
            }),
            verbose: false,
        })
        .await
        .expect("ImageStatus failed")
        .into_inner();
    assert!(gone.image.is_none(), "{gone:?}");
}
