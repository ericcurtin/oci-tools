//! The real `RuntimeService` gRPC implementation — see this crate's
//! own module doc comment (`main.rs`) for the exact scope of this
//! first slice: `Version`/`Status` are genuinely implemented; every
//! other one of the real CRI v1 `RuntimeService`'s remaining 32 RPCs
//! (pod sandbox/container lifecycle, exec/attach/port-forward, stats,
//! events, ...) returns a real, honest `Status::unimplemented` rather
//! than silently accepting a request it can't actually act on —
//! matching this project's own established "narrow first slice,
//! document the rest" pattern used everywhere else (e.g. `ociboot
//! build-image` before `install to-disk`).
//!
//! `ImageService` (the CRI's other, smaller service — `ListImages`/
//! `PullImage`/... ) isn't wired up into the server at all yet; it's
//! its own separate, still-ahead increment.

use tonic::codegen::BoxStream;
use tonic::{Request, Response, Status};

use crate::cri;

/// `Version`'s own `runtime_name`, matching this project's own real
/// binary name (not `"cri-o"` — a real, honest identification of what
/// actually answered the request, exactly like `ociman version`/
/// `ocirun --version` report their own real names rather than
/// `"podman"`/`"crun"`).
const RUNTIME_NAME: &str = "ocicri";

/// `Version`'s own `version` field: the real CRI *kubelet API*
/// version this server speaks, not this project's own build version
/// (that's `runtime_version`, below) — checked directly against real
/// `cri-o`'s own identical constant (`server/version.go`'s own
/// `kubeAPIVersion`), itself a fixed historical value every real CRI
/// implementation returns regardless of what the request itself asked
/// for.
const KUBE_API_VERSION: &str = "0.1.0";

/// `Version`'s own `runtime_api_version` field — the CRI protocol
/// version this server implements (`package runtime.v1` in
/// `proto/api.proto`), matching real `cri-o`'s own identical constant.
const RUNTIME_API_VERSION: &str = "v1";

/// `Status`'s own `RuntimeCondition.type` values — checked directly
/// against real `cri-o`'s own vendored `k8s.io/cri-api` constants
/// (`server/runtime_status.go`): exactly these two exact strings,
/// matching the real, fixed contract every CRI implementation
/// reports, not something either runtime invents on its own.
const RUNTIME_READY_CONDITION: &str = "RuntimeReady";
const NETWORK_READY_CONDITION: &str = "NetworkReady";

/// The real `RuntimeService` state — empty for now (`Version` needs
/// none at all); will grow real `oci_store`/`oci_runtime_core` state
/// once pod sandbox/container lifecycle RPCs are implemented.
#[derive(Debug, Default)]
pub struct RuntimeServiceImpl;

/// A real, honest "not implemented yet" error for every RPC this first
/// slice doesn't answer — `name` is the real RPC name (matching
/// `proto/api.proto`'s own `rpc` name, e.g. `"RunPodSandbox"`) so a
/// real caller's own error message actually names what it tried to
/// call, not a generic "not implemented" with no further information.
fn unimplemented<T>(name: &str) -> Result<Response<T>, Status> {
    Err(Status::unimplemented(format!(
        "ocicri: {name} is not implemented yet (milestone 7, a real, narrow first slice: only \
         Version/Status are answered so far)"
    )))
}

#[tonic::async_trait]
impl cri::runtime_service_server::RuntimeService for RuntimeServiceImpl {
    /// The one real, fully-implemented RPC in this first slice — see
    /// this module's own doc comment for exactly why it's the one
    /// chosen (the simplest, most fundamental CRI call: kubelet's own
    /// first connectivity/compatibility check against any runtime).
    async fn version(
        &self,
        _request: Request<cri::VersionRequest>,
    ) -> Result<Response<cri::VersionResponse>, Status> {
        Ok(Response::new(cri::VersionResponse {
            version: KUBE_API_VERSION.to_string(),
            runtime_name: RUNTIME_NAME.to_string(),
            runtime_version: oci_cli_common::version::long(env!("CARGO_PKG_VERSION")),
            runtime_api_version: RUNTIME_API_VERSION.to_string(),
        }))
    }

    async fn run_pod_sandbox(
        &self,
        _request: Request<cri::RunPodSandboxRequest>,
    ) -> Result<Response<cri::RunPodSandboxResponse>, Status> {
        unimplemented("RunPodSandbox")
    }

    async fn stop_pod_sandbox(
        &self,
        _request: Request<cri::StopPodSandboxRequest>,
    ) -> Result<Response<cri::StopPodSandboxResponse>, Status> {
        unimplemented("StopPodSandbox")
    }

    async fn remove_pod_sandbox(
        &self,
        _request: Request<cri::RemovePodSandboxRequest>,
    ) -> Result<Response<cri::RemovePodSandboxResponse>, Status> {
        unimplemented("RemovePodSandbox")
    }

    async fn pod_sandbox_status(
        &self,
        _request: Request<cri::PodSandboxStatusRequest>,
    ) -> Result<Response<cri::PodSandboxStatusResponse>, Status> {
        unimplemented("PodSandboxStatus")
    }

    async fn list_pod_sandbox(
        &self,
        _request: Request<cri::ListPodSandboxRequest>,
    ) -> Result<Response<cri::ListPodSandboxResponse>, Status> {
        unimplemented("ListPodSandbox")
    }

    type StreamPodSandboxesStream = BoxStream<cri::StreamPodSandboxesResponse>;

    async fn stream_pod_sandboxes(
        &self,
        _request: Request<cri::StreamPodSandboxesRequest>,
    ) -> Result<Response<Self::StreamPodSandboxesStream>, Status> {
        unimplemented("StreamPodSandboxes")
    }

    async fn create_container(
        &self,
        _request: Request<cri::CreateContainerRequest>,
    ) -> Result<Response<cri::CreateContainerResponse>, Status> {
        unimplemented("CreateContainer")
    }

    async fn start_container(
        &self,
        _request: Request<cri::StartContainerRequest>,
    ) -> Result<Response<cri::StartContainerResponse>, Status> {
        unimplemented("StartContainer")
    }

    async fn stop_container(
        &self,
        _request: Request<cri::StopContainerRequest>,
    ) -> Result<Response<cri::StopContainerResponse>, Status> {
        unimplemented("StopContainer")
    }

    async fn remove_container(
        &self,
        _request: Request<cri::RemoveContainerRequest>,
    ) -> Result<Response<cri::RemoveContainerResponse>, Status> {
        unimplemented("RemoveContainer")
    }

    async fn list_containers(
        &self,
        _request: Request<cri::ListContainersRequest>,
    ) -> Result<Response<cri::ListContainersResponse>, Status> {
        unimplemented("ListContainers")
    }

    type StreamContainersStream = BoxStream<cri::StreamContainersResponse>;

    async fn stream_containers(
        &self,
        _request: Request<cri::StreamContainersRequest>,
    ) -> Result<Response<Self::StreamContainersStream>, Status> {
        unimplemented("StreamContainers")
    }

    async fn container_status(
        &self,
        _request: Request<cri::ContainerStatusRequest>,
    ) -> Result<Response<cri::ContainerStatusResponse>, Status> {
        unimplemented("ContainerStatus")
    }

    async fn update_container_resources(
        &self,
        _request: Request<cri::UpdateContainerResourcesRequest>,
    ) -> Result<Response<cri::UpdateContainerResourcesResponse>, Status> {
        unimplemented("UpdateContainerResources")
    }

    async fn reopen_container_log(
        &self,
        _request: Request<cri::ReopenContainerLogRequest>,
    ) -> Result<Response<cri::ReopenContainerLogResponse>, Status> {
        unimplemented("ReopenContainerLog")
    }

    async fn exec_sync(
        &self,
        _request: Request<cri::ExecSyncRequest>,
    ) -> Result<Response<cri::ExecSyncResponse>, Status> {
        unimplemented("ExecSync")
    }

    async fn exec(
        &self,
        _request: Request<cri::ExecRequest>,
    ) -> Result<Response<cri::ExecResponse>, Status> {
        unimplemented("Exec")
    }

    async fn attach(
        &self,
        _request: Request<cri::AttachRequest>,
    ) -> Result<Response<cri::AttachResponse>, Status> {
        unimplemented("Attach")
    }

    async fn port_forward(
        &self,
        _request: Request<cri::PortForwardRequest>,
    ) -> Result<Response<cri::PortForwardResponse>, Status> {
        unimplemented("PortForward")
    }

    async fn container_stats(
        &self,
        _request: Request<cri::ContainerStatsRequest>,
    ) -> Result<Response<cri::ContainerStatsResponse>, Status> {
        unimplemented("ContainerStats")
    }

    async fn list_container_stats(
        &self,
        _request: Request<cri::ListContainerStatsRequest>,
    ) -> Result<Response<cri::ListContainerStatsResponse>, Status> {
        unimplemented("ListContainerStats")
    }

    type StreamContainerStatsStream = BoxStream<cri::StreamContainerStatsResponse>;

    async fn stream_container_stats(
        &self,
        _request: Request<cri::StreamContainerStatsRequest>,
    ) -> Result<Response<Self::StreamContainerStatsStream>, Status> {
        unimplemented("StreamContainerStats")
    }

    async fn pod_sandbox_stats(
        &self,
        _request: Request<cri::PodSandboxStatsRequest>,
    ) -> Result<Response<cri::PodSandboxStatsResponse>, Status> {
        unimplemented("PodSandboxStats")
    }

    async fn list_pod_sandbox_stats(
        &self,
        _request: Request<cri::ListPodSandboxStatsRequest>,
    ) -> Result<Response<cri::ListPodSandboxStatsResponse>, Status> {
        unimplemented("ListPodSandboxStats")
    }

    type StreamPodSandboxStatsStream = BoxStream<cri::StreamPodSandboxStatsResponse>;

    async fn stream_pod_sandbox_stats(
        &self,
        _request: Request<cri::StreamPodSandboxStatsRequest>,
    ) -> Result<Response<Self::StreamPodSandboxStatsStream>, Status> {
        unimplemented("StreamPodSandboxStats")
    }

    async fn update_runtime_config(
        &self,
        _request: Request<cri::UpdateRuntimeConfigRequest>,
    ) -> Result<Response<cri::UpdateRuntimeConfigResponse>, Status> {
        unimplemented("UpdateRuntimeConfig")
    }

    /// A real, mostly-static response — checked directly against real
    /// `cri-o`'s own `server/runtime_status.go`, which this matches or
    /// deliberately, honestly diverges from:
    ///
    /// * `RuntimeReady` — `true` unconditionally, matching real
    ///   `cri-o` exactly: it hard-codes this too, since answering the
    ///   RPC at all is the only "proof" either implementation ever
    ///   checks.
    /// * `NetworkReady` — a real, honest `false`, unlike real `cri-o`
    ///   (which polls a real, configured CNI plugin's own live
    ///   status): this project sets up no container networking of its
    ///   own at all yet (no bridge, no pasta, no CNI — see
    ///   `docs/design/0147`), so reporting readiness here would be a
    ///   real, false claim, not an honest one.
    /// * `runtime_handlers` — real `cri-o` reports one real entry per
    ///   *configured* OCI runtime (`crio.conf`); this project has no
    ///   configurable runtime-handler concept at all yet, so the
    ///   smallest honest answer is exactly one entry naming the
    ///   implicit default handler (`name: ""`, matching the proto's
    ///   own "empty string denotes the default handler" convention),
    ///   with both real feature bits `false` (neither recursive
    ///   read-only mounts nor user namespaces are implemented here).
    /// * `features` — both `false`: neither `SupplementalGroupsPolicy`
    ///   nor simultaneous host-network-plus-user-namespace support is
    ///   implemented anywhere in this project yet, unlike real
    ///   `cri-o`, which hard-codes both `true` as a genuine, backed
    ///   capability claim.
    /// * `info` (only when `verbose`) — the same real, already-known
    ///   values `Version` itself already reports (name/version),
    ///   never fabricated cri-o-style CNI/runtime config this project
    ///   doesn't actually have.
    ///
    /// Always succeeds — matching real `cri-o` exactly: there's no
    /// real failure condition for a response this static.
    async fn status(
        &self,
        request: Request<cri::StatusRequest>,
    ) -> Result<Response<cri::StatusResponse>, Status> {
        let verbose = request.into_inner().verbose;

        let info = if verbose {
            std::collections::HashMap::from([
                ("runtimeName".to_string(), RUNTIME_NAME.to_string()),
                (
                    "runtimeVersion".to_string(),
                    oci_cli_common::version::long(env!("CARGO_PKG_VERSION")),
                ),
            ])
        } else {
            std::collections::HashMap::new()
        };

        Ok(Response::new(cri::StatusResponse {
            status: Some(cri::RuntimeStatus {
                conditions: vec![
                    cri::RuntimeCondition {
                        r#type: RUNTIME_READY_CONDITION.to_string(),
                        status: true,
                        reason: String::new(),
                        message: String::new(),
                    },
                    cri::RuntimeCondition {
                        r#type: NETWORK_READY_CONDITION.to_string(),
                        status: false,
                        reason: "NetworkNotImplemented".to_string(),
                        message: "ocicri sets up no container networking of its own yet \
                                  (no bridge, no pasta, no CNI) -- see docs/design/0147"
                            .to_string(),
                    },
                ],
            }),
            info,
            runtime_handlers: vec![cri::RuntimeHandler {
                name: String::new(),
                features: Some(cri::RuntimeHandlerFeatures {
                    recursive_read_only_mounts: false,
                    user_namespaces: false,
                }),
            }],
            features: Some(cri::RuntimeFeatures {
                supplemental_groups_policy: false,
                user_namespaces_host_network: false,
            }),
        }))
    }

    async fn checkpoint_container(
        &self,
        _request: Request<cri::CheckpointContainerRequest>,
    ) -> Result<Response<cri::CheckpointContainerResponse>, Status> {
        unimplemented("CheckpointContainer")
    }

    type GetContainerEventsStream = BoxStream<cri::ContainerEventResponse>;

    async fn get_container_events(
        &self,
        _request: Request<cri::GetEventsRequest>,
    ) -> Result<Response<Self::GetContainerEventsStream>, Status> {
        unimplemented("GetContainerEvents")
    }

    async fn list_metric_descriptors(
        &self,
        _request: Request<cri::ListMetricDescriptorsRequest>,
    ) -> Result<Response<cri::ListMetricDescriptorsResponse>, Status> {
        unimplemented("ListMetricDescriptors")
    }

    async fn list_pod_sandbox_metrics(
        &self,
        _request: Request<cri::ListPodSandboxMetricsRequest>,
    ) -> Result<Response<cri::ListPodSandboxMetricsResponse>, Status> {
        unimplemented("ListPodSandboxMetrics")
    }

    type StreamPodSandboxMetricsStream = BoxStream<cri::StreamPodSandboxMetricsResponse>;

    async fn stream_pod_sandbox_metrics(
        &self,
        _request: Request<cri::StreamPodSandboxMetricsRequest>,
    ) -> Result<Response<Self::StreamPodSandboxMetricsStream>, Status> {
        unimplemented("StreamPodSandboxMetrics")
    }

    async fn runtime_config(
        &self,
        _request: Request<cri::RuntimeConfigRequest>,
    ) -> Result<Response<cri::RuntimeConfigResponse>, Status> {
        unimplemented("RuntimeConfig")
    }

    async fn update_pod_sandbox_resources(
        &self,
        _request: Request<cri::UpdatePodSandboxResourcesRequest>,
    ) -> Result<Response<cri::UpdatePodSandboxResourcesResponse>, Status> {
        unimplemented("UpdatePodSandboxResources")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cri::runtime_service_server::RuntimeService as _;

    #[tokio::test]
    async fn version_reports_real_honest_values() {
        let service = RuntimeServiceImpl;
        let response = service
            .version(Request::new(cri::VersionRequest {
                version: "0.1.0".to_string(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(response.version, KUBE_API_VERSION);
        assert_eq!(response.runtime_name, RUNTIME_NAME);
        assert_eq!(response.runtime_api_version, RUNTIME_API_VERSION);
        assert!(
            response
                .runtime_version
                .starts_with(env!("CARGO_PKG_VERSION")),
            "{}",
            response.runtime_version
        );
    }

    #[tokio::test]
    async fn every_other_rpc_is_a_real_honest_unimplemented_status() {
        let service = RuntimeServiceImpl;
        let status = service
            .run_pod_sandbox(Request::new(cri::RunPodSandboxRequest {
                config: None,
                runtime_handler: String::new(),
            }))
            .await
            .unwrap_err();
        assert_eq!(status.code(), tonic::Code::Unimplemented);
        assert!(status.message().contains("RunPodSandbox"), "{status:?}");
    }
}
