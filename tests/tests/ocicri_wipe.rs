//! `ocicri wipe` (`docs/design/0542`): a real, local, human-run CLI
//! subcommand -- removes every stored pod-sandbox/container record and
//! its bundle. Matches real `crio wipe`'s own checked-directly
//! semantics for the part this project's own architecture can safely
//! act on (see `Command::Wipe`'s own doc comment for the exact,
//! honest narrowing versus real crio: no image wipe, `--force` a real
//! no-op).
//!
//! Every test here runs `ocicri wipe` only *after* killing the real
//! server (matching real crio's own primary invocation model, a
//! systemd `ExecStartPre` before the server itself starts -- see
//! `cmd_wipe`'s own doc comment), then respawns a fresh server against
//! the same storage root to prove the wipe genuinely persisted to
//! disk, not just some in-memory state.

use std::collections::HashMap;
use std::path::Path;
use std::process::{Child, Command};
use std::time::Duration;

use oci_cri_types::runtime_service_client::RuntimeServiceClient;
use oci_cri_types::{
    ContainerConfig as CriContainerConfig, ContainerMetadata, CreateContainerRequest, ImageSpec,
    ListContainersRequest, ListPodSandboxRequest, PodSandboxConfig, PodSandboxMetadata,
    RunPodSandboxRequest,
};
use oci_spec_types::image::ContainerConfig;
use oci_store::Store;
use oci_tools_tests::{bin_path, busybox_path, seed_image};

const IMAGE: &str = "docker.io/ocicri-test/wipe-base:latest";

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

fn container_config(name: &str) -> CriContainerConfig {
    CriContainerConfig {
        metadata: Some(ContainerMetadata {
            name: name.to_string(),
            attempt: 0,
        }),
        image: Some(ImageSpec {
            image: IMAGE.to_string(),
            ..Default::default()
        }),
        command: vec!["/bin/sh".to_string()],
        labels: HashMap::from([("app".to_string(), name.to_string())]),
        ..Default::default()
    }
}

fn bundle_dir(storage_root: &Path, container_id: &str) -> std::path::PathBuf {
    storage_root.join("cri-bundles").join(container_id)
}

fn ocicri_cli(storage_root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin_path("ocicri"))
        .env("OCI_TOOLS_STORAGE_ROOT", storage_root)
        .env_remove("OCI_TOOLS_LOG")
        .args(args)
        .output()
        .expect("failed to spawn ocicri")
}

#[test]
fn wipe_on_an_empty_store_succeeds_silently() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ocicri_cli(storage_dir.path(), &["wipe"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.is_empty(),
        "nothing to delete should print nothing: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn wipe_json_on_an_empty_store_reports_two_empty_lists() {
    let storage_dir = tempfile::tempdir().unwrap();
    let out = ocicri_cli(storage_dir.path(), &["wipe", "--json"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("wipe --json output was not valid JSON: {e}"));
    assert_eq!(report["containers"], serde_json::json!([]));
    assert_eq!(report["pod_sandboxes"], serde_json::json!([]));
}

/// `--force`/`-f` is a real, faithful no-op (see `Command::Wipe`'s own
/// doc comment for why): every invocation already wipes
/// unconditionally, whether given or not.
#[tokio::test]
async fn wipe_force_flag_is_accepted_and_behaves_identically() {
    let storage_dir = tempfile::tempdir().unwrap();
    let socket_dir = tempfile::tempdir().unwrap();
    let socket_path = socket_dir.path().join("ocicri.sock");
    {
        let server = spawn_server(storage_dir.path(), &socket_path);
        wait_for_socket(&socket_path);
        let mut client = connect(socket_path.clone()).await;
        client
            .run_pod_sandbox(RunPodSandboxRequest {
                config: Some(pod_config("force-flag", "uid-1")),
                runtime_handler: String::new(),
            })
            .await
            .expect("RunPodSandbox failed");
        drop(server);
    }

    let out = ocicri_cli(storage_dir.path(), &["wipe", "--force"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Deleted pod sandbox"),
        "--force should wipe exactly like a plain `wipe`: {stdout:?}"
    );
}

/// Every real, stored pod-sandbox record (and only those -- no
/// container was ever created here) is removed, verified by
/// respawning a fresh server against the same storage root and
/// listing again.
#[tokio::test]
async fn wipe_removes_every_pod_sandbox_record() {
    let storage_dir = tempfile::tempdir().unwrap();
    let socket_dir = tempfile::tempdir().unwrap();
    let socket_path = socket_dir.path().join("ocicri.sock");
    let sandbox_ids = {
        let server = spawn_server(storage_dir.path(), &socket_path);
        wait_for_socket(&socket_path);
        let mut client = connect(socket_path.clone()).await;
        let mut ids = Vec::new();
        for (name, uid) in [("web", "uid-1"), ("db", "uid-2")] {
            let id = client
                .run_pod_sandbox(RunPodSandboxRequest {
                    config: Some(pod_config(name, uid)),
                    runtime_handler: String::new(),
                })
                .await
                .expect("RunPodSandbox failed")
                .into_inner()
                .pod_sandbox_id;
            ids.push(id);
        }
        drop(server);
        ids
    };

    let wipe = ocicri_cli(storage_dir.path(), &["wipe"]);
    assert!(
        wipe.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&wipe.stderr)
    );
    let stdout = String::from_utf8_lossy(&wipe.stdout);
    for id in &sandbox_ids {
        assert!(
            stdout.contains(&format!("Deleted pod sandbox {id}")),
            "stdout: {stdout:?}"
        );
    }

    // Respawn: the wipe genuinely persisted to disk.
    let socket_dir2 = tempfile::tempdir().unwrap();
    let socket_path2 = socket_dir2.path().join("ocicri.sock");
    let server2 = spawn_server(storage_dir.path(), &socket_path2);
    wait_for_socket(&socket_path2);
    let mut client2 = connect(socket_path2).await;
    let remaining = client2
        .list_pod_sandbox(ListPodSandboxRequest { filter: None })
        .await
        .expect("ListPodSandbox failed")
        .into_inner()
        .items;
    assert!(remaining.is_empty(), "{remaining:?}");
    drop(server2);
}

/// Every real, stored container record's bundle (a real extracted
/// rootfs plus `config.json`, `docs/design/0237`) is removed along
/// with its record -- verified both on disk directly and by
/// respawning a fresh server and listing again.
#[tokio::test]
async fn wipe_removes_every_container_record_and_its_bundle() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(&store, IMAGE, &busybox, &["sh"], ContainerConfig::default());

    let socket_dir = tempfile::tempdir().unwrap();
    let socket_path = socket_dir.path().join("ocicri.sock");
    let container_id = {
        let server = spawn_server(storage_dir.path(), &socket_path);
        wait_for_socket(&socket_path);
        let mut client = connect(socket_path.clone()).await;
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
        let container_id = client
            .create_container(CreateContainerRequest {
                pod_sandbox_id: sandbox_id,
                config: Some(container_config("app")),
                sandbox_config: Some(sandbox_config),
            })
            .await
            .expect("CreateContainer failed")
            .into_inner()
            .container_id;
        assert!(
            bundle_dir(storage_dir.path(), &container_id)
                .join("rootfs/bin/sh")
                .exists(),
            "sanity: the bundle should be a real extraction before wipe"
        );
        drop(server);
        container_id
    };

    let wipe = ocicri_cli(storage_dir.path(), &["wipe"]);
    assert!(
        wipe.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&wipe.stderr)
    );
    let stdout = String::from_utf8_lossy(&wipe.stdout);
    assert!(
        stdout.contains(&format!("Deleted container {container_id}")),
        "stdout: {stdout:?}"
    );
    assert!(
        !bundle_dir(storage_dir.path(), &container_id).exists(),
        "wipe should remove the container's own bundle directory too"
    );

    let socket_dir2 = tempfile::tempdir().unwrap();
    let socket_path2 = socket_dir2.path().join("ocicri.sock");
    let server2 = spawn_server(storage_dir.path(), &socket_path2);
    wait_for_socket(&socket_path2);
    let mut client2 = connect(socket_path2).await;
    let remaining_containers = client2
        .list_containers(ListContainersRequest { filter: None })
        .await
        .expect("ListContainers failed")
        .into_inner()
        .containers;
    assert!(remaining_containers.is_empty(), "{remaining_containers:?}");
    let remaining_sandboxes = client2
        .list_pod_sandbox(ListPodSandboxRequest { filter: None })
        .await
        .expect("ListPodSandbox failed")
        .into_inner()
        .items;
    assert!(remaining_sandboxes.is_empty(), "{remaining_sandboxes:?}");
    drop(server2);
}

/// `--json` reports exactly the removed IDs -- for both record
/// families at once, matching a real, mixed store.
#[tokio::test]
async fn wipe_json_reports_every_removed_container_and_pod_sandbox_id() {
    let Some(busybox) = busybox_path() else {
        eprintln!("skipping: busybox not found on $PATH");
        return;
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let store = Store::open(storage_dir.path()).unwrap();
    seed_image(&store, IMAGE, &busybox, &["sh"], ContainerConfig::default());

    let socket_dir = tempfile::tempdir().unwrap();
    let socket_path = socket_dir.path().join("ocicri.sock");
    let (sandbox_id, container_id) = {
        let server = spawn_server(storage_dir.path(), &socket_path);
        wait_for_socket(&socket_path);
        let mut client = connect(socket_path.clone()).await;
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
        let container_id = client
            .create_container(CreateContainerRequest {
                pod_sandbox_id: sandbox_id.clone(),
                config: Some(container_config("app")),
                sandbox_config: Some(sandbox_config),
            })
            .await
            .expect("CreateContainer failed")
            .into_inner()
            .container_id;
        drop(server);
        (sandbox_id, container_id)
    };

    let wipe = ocicri_cli(storage_dir.path(), &["wipe", "--json"]);
    assert!(
        wipe.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&wipe.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&wipe.stdout)
        .unwrap_or_else(|e| panic!("wipe --json output was not valid JSON: {e}"));
    assert_eq!(report["containers"], serde_json::json!([container_id]));
    assert_eq!(report["pod_sandboxes"], serde_json::json!([sandbox_id]));
}
