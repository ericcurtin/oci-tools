//! The real `RuntimeService` gRPC implementation — `Version`/
//! `Status`/`RuntimeConfig`/`UpdateRuntimeConfig`/
//! `ListMetricDescriptors` plus the full pod-sandbox lifecycle
//! (`RunPodSandbox`/`StopPodSandbox`/`RemovePodSandbox`/
//! `PodSandboxStatus`/`ListPodSandbox`/`StreamPodSandboxes`, see
//! `docs/design/0233`-`0234` and `sandbox.rs`'s own module doc
//! comment for exactly what a sandbox is — and honestly isn't — here
//! yet) are genuinely implemented;
//! every other one of the real CRI v1 `RuntimeService`'s remaining
//! RPCs (container lifecycle, exec/attach/port-forward, stats,
//! events, ...) returns a real, honest `Status::unimplemented` rather
//! than silently accepting a request it can't actually act on —
//! matching this project's own established "narrow first slice,
//! document the rest" pattern used everywhere else (e.g. `ociboot
//! build-image` before `install to-disk`).

use tonic::codegen::BoxStream;
use tonic::{Request, Response, Status};

use crate::container;
use crate::cri;
use crate::sandbox;

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

/// The kubelet-default labels `populateSandboxLabels` (real cri-o,
/// `server/sandbox_run_linux.go`) fills in when a client (`crictl`)
/// didn't — checked directly against the real
/// `k8s.io/kubelet/pkg/types` constants.
const POD_NAME_LABEL: &str = "io.kubernetes.pod.name";
const POD_NAMESPACE_LABEL: &str = "io.kubernetes.pod.namespace";
const POD_UID_LABEL: &str = "io.kubernetes.pod.uid";

/// The real `RuntimeService` state: one lock serializing mutating
/// pod-sandbox RPCs, so two concurrent `RunPodSandbox` calls with the
/// same metadata can't both miss the duplicate-name check and write
/// two records for one pod (real cri-o's own equivalent is its
/// name-registrar's `ReservePodName`). Reads (`PodSandboxStatus`/
/// `ListPodSandbox`) stay lock-free plain file reads, the same model
/// `ImageService` already uses against `oci_store`.
#[derive(Debug, Default)]
pub struct RuntimeServiceImpl {
    sandbox_mutation_lock: std::sync::Mutex<()>,
}

/// A real, honest "not implemented yet" error for every RPC this first
/// slice doesn't answer — `name` is the real RPC name (matching
/// `proto/api.proto`'s own `rpc` name, e.g. `"CreateContainer"`) so a
/// real caller's own error message actually names what it tried to
/// call, not a generic "not implemented" with no further information.
fn unimplemented<T>(name: &str) -> Result<Response<T>, Status> {
    Err(Status::unimplemented(format!(
        "ocicri: {name} is not implemented yet (milestone 7: Version/Status/RuntimeConfig/\
         UpdateRuntimeConfig/ListMetricDescriptors, the pod-sandbox lifecycle, and the container \
         lifecycle (create/start/stop/remove/status/list) are answered so far)"
    )))
}

/// The sandbox record directory under this process's own real storage
/// root — resolved per call, like `ImageService`'s own `open_store`,
/// so tests can point one spawned server at its own private root via
/// `OCI_TOOLS_STORAGE_ROOT`.
fn sandbox_store_root() -> std::path::PathBuf {
    sandbox::sandbox_root(&oci_cli_common::storage::default_root())
}

/// The container record directory, same resolution rules as
/// [`sandbox_store_root`].
fn container_store_root() -> std::path::PathBuf {
    container::container_root(&oci_cli_common::storage::default_root())
}

fn io_error(context: &str, e: std::io::Error) -> Status {
    Status::internal(format!("{context}: {e}"))
}

fn now_nanos() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

fn state_to_proto(state: sandbox::SandboxState) -> i32 {
    match state {
        sandbox::SandboxState::Ready => cri::PodSandboxState::SandboxReady as i32,
        sandbox::SandboxState::NotReady => cri::PodSandboxState::SandboxNotready as i32,
    }
}

fn metadata_to_proto(metadata: &sandbox::SandboxMetadata) -> cri::PodSandboxMetadata {
    cri::PodSandboxMetadata {
        name: metadata.name.clone(),
        uid: metadata.uid.clone(),
        namespace: metadata.namespace.clone(),
        attempt: metadata.attempt,
    }
}

/// Resolves one sandbox for a mutating/status RPC. `Ok(None)` is the
/// real "not found" case each caller maps per its own real cri-o
/// semantics (silent success for stop/remove, `NotFound` for status —
/// see `docs/design/0233`); an ambiguous prefix is a client-input
/// problem (`InvalidArgument`), distinct from both.
fn find_sandbox(id: &str) -> Result<Option<sandbox::SandboxRecord>, Status> {
    match sandbox::find_by_id_prefix(&sandbox_store_root(), id) {
        Ok(found) => Ok(found),
        Err(sandbox::LookupError::AmbiguousPrefix(prefix)) => Err(Status::invalid_argument(
            format!("sandbox ID {prefix:?} is ambiguous: matches more than one sandbox"),
        )),
        Err(sandbox::LookupError::Io(e)) => Err(io_error("reading sandbox records", e)),
    }
}

/// Whether `record` passes the given list filter's `state`/
/// `label_selector` criteria (ANDed, matching real cri-o's own
/// `filterSandbox`: a state filter compares exactly; a label selector
/// requires every given key/value pair to match).
fn matches_filter(record: &sandbox::SandboxRecord, filter: &cri::PodSandboxFilter) -> bool {
    if let Some(state) = &filter.state
        && state.state != state_to_proto(record.state)
    {
        return false;
    }
    filter
        .label_selector
        .iter()
        .all(|(k, v)| record.labels.get(k) == Some(v))
}

/// The one real filtered-list computation behind both
/// `ListPodSandbox` and its `CRIListStreaming` sibling
/// `StreamPodSandboxes` — factored out (a pure, behavior-preserving
/// move, `docs/design/0234`) exactly like real cri-o's own shared
/// `listPodSandboxes` helper serving both of its RPCs. Filters
/// combine with AND (`filterSandboxList`/`filterSandbox`): an `id`
/// filter that matches nothing (or is ambiguous) yields an empty
/// list, never an error.
fn sandbox_list_items(
    filter: Option<cri::PodSandboxFilter>,
) -> Result<Vec<cri::PodSandbox>, Status> {
    let records = match filter.as_ref().map(|f| f.id.as_str()) {
        Some(id) if !id.is_empty() => {
            match sandbox::find_by_id_prefix(&sandbox_store_root(), id) {
                Ok(Some(record)) => vec![record],
                // "Not finding an ID in a filtered list should not
                // be considered an error" (real cri-o's own
                // comment) -- and its truncindex returns an error
                // for an ambiguous prefix, which lands in the same
                // warn-and-return-empty path.
                Ok(None) | Err(sandbox::LookupError::AmbiguousPrefix(_)) => Vec::new(),
                Err(sandbox::LookupError::Io(e)) => {
                    return Err(io_error("reading sandbox records", e));
                }
            }
        }
        _ => sandbox::load_all(&sandbox_store_root())
            .map_err(|e| io_error("reading sandbox records", e))?,
    };

    Ok(records
        .into_iter()
        .filter(|record| {
            filter
                .as_ref()
                .is_none_or(|filter| matches_filter(record, filter))
        })
        .map(|record| cri::PodSandbox {
            id: record.id.clone(),
            metadata: Some(metadata_to_proto(&record.metadata)),
            state: state_to_proto(record.state),
            created_at: record.created_at_nanos,
            labels: record.labels,
            annotations: record.annotations,
            runtime_handler: String::new(),
        })
        .collect())
}

/// Whether a process with this pid is currently alive (the same
/// `kill(pid, 0)`-based check `oci_runtime_core::process::alive`
/// provides, shared with `ociman`'s own status logic).
fn pid_alive(pid: i32) -> bool {
    oci_runtime_core::process::alive(pid)
}

/// Brings one container record up to date with what its launcher-
/// keeper actually recorded (`docs/design/0238`): a `RUNNING` record
/// whose launcher has written `exit.json` (or whose pid is simply
/// gone) becomes `EXITED`, persisted. Callers hold the mutation lock.
/// Everything else passes through unchanged — `CREATED` records have
/// no process to reconcile against, and `EXITED` is terminal.
fn reconcile_container(
    mut record: container::ContainerRecord,
) -> Result<container::ContainerRecord, Status> {
    if record.state != container::ContainerState::Running {
        return Ok(record);
    }
    let bundle_dir =
        crate::bundle::bundle_dir(&oci_cli_common::storage::default_root(), &record.id);
    let exit = crate::launcher::read_exit(&bundle_dir)
        .map_err(|e| Status::internal(format!("reading exit record: {e}")))?;
    match exit {
        Some(exit) => {
            record.state = container::ContainerState::Exited;
            record.exit_code = Some(exit.exit_code);
            record.finished_at_nanos = Some(exit.finished_at_nanos);
        }
        None => {
            let alive = record.pid.is_some_and(pid_alive);
            if alive {
                return Ok(record);
            }
            // The pid is gone but no exit record exists (yet). The
            // launcher writes it moments after the container dies --
            // give it a real chance before declaring the code lost
            // (real cri-o's own status path re-polls its own exit
            // files the same way).
            for _ in 0..20 {
                std::thread::sleep(std::time::Duration::from_millis(50));
                if let Some(exit) = crate::launcher::read_exit(&bundle_dir)
                    .map_err(|e| Status::internal(format!("reading exit record: {e}")))?
                {
                    record.state = container::ContainerState::Exited;
                    record.exit_code = Some(exit.exit_code);
                    record.finished_at_nanos = Some(exit.finished_at_nanos);
                    container::save(&container_store_root(), &record)
                        .map_err(|e| io_error("saving container record", e))?;
                    return Ok(record);
                }
            }
            // Genuinely lost (launcher itself killed before it could
            // record anything): exited, code unknown -- reported as
            // -1, real cri-o's own identical `ExitCode == nil`
            // fallback.
            record.state = container::ContainerState::Exited;
            record.exit_code = None;
            record.finished_at_nanos = Some(now_nanos());
        }
    }
    container::save(&container_store_root(), &record)
        .map_err(|e| io_error("saving container record", e))?;
    Ok(record)
}

/// Force-terminates one container's process if it's still running and
/// waits for the launcher's exit record — the forceful half shared by
/// `RemoveContainer` (the proto: running containers "must be forcibly
/// ... removed"), `StopPodSandbox` and `RemovePodSandbox`'s container
/// cascades. Idempotent for anything not running.
fn force_kill_and_reconcile(
    record: container::ContainerRecord,
) -> Result<container::ContainerRecord, Status> {
    let record = reconcile_container(record)?;
    if record.state != container::ContainerState::Running {
        return Ok(record);
    }
    if let Some(pid) = record.pid {
        // SIGKILL straight away -- this is the forceful path. The
        // same numeric-signal `kill(2)` wrapper `ociman kill` uses.
        let _ = oci_runtime_core::process::kill(pid, libc::SIGKILL);
    }
    // The kill is asynchronous; wait for the launcher to record the
    // exit (bounded -- SIGKILL cannot be ignored, so this converges
    // fast in practice).
    for _ in 0..100 {
        let reconciled = reconcile_container(record.clone())?;
        if reconciled.state != container::ContainerState::Running {
            return Ok(reconciled);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Err(Status::internal(format!(
        "container {} did not exit after SIGKILL",
        record.id
    )))
}

fn container_state_to_proto(state: container::ContainerState) -> i32 {
    match state {
        container::ContainerState::Created => cri::ContainerState::ContainerCreated as i32,
        container::ContainerState::Running => cri::ContainerState::ContainerRunning as i32,
        container::ContainerState::Exited => cri::ContainerState::ContainerExited as i32,
    }
}

fn container_metadata_to_proto(metadata: &container::ContainerMetadata) -> cri::ContainerMetadata {
    cri::ContainerMetadata {
        name: metadata.name.clone(),
        attempt: metadata.attempt,
    }
}

/// Resolves one container for a mutating/status RPC — the container
/// counterpart of [`find_sandbox`], with the identical per-caller
/// "not found" mapping rules (`docs/design/0236`).
fn find_container(id: &str) -> Result<Option<container::ContainerRecord>, Status> {
    match container::find_by_id_prefix(&container_store_root(), id) {
        Ok(found) => Ok(found),
        Err(container::LookupError::AmbiguousPrefix(prefix)) => Err(Status::invalid_argument(
            format!("container ID {prefix:?} is ambiguous: matches more than one container"),
        )),
        Err(container::LookupError::Io(e)) => Err(io_error("reading container records", e)),
    }
}

/// Builds the CRI `Container` list message for one record.
fn container_to_proto(record: container::ContainerRecord) -> cri::Container {
    cri::Container {
        id: record.id.clone(),
        pod_sandbox_id: record.sandbox_id.clone(),
        metadata: Some(container_metadata_to_proto(&record.metadata)),
        image: Some(cri::ImageSpec {
            image: record.image.clone(),
            ..Default::default()
        }),
        image_ref: record.image_ref.clone(),
        image_id: record.image_ref.clone(),
        state: container_state_to_proto(record.state),
        created_at: record.created_at_nanos,
        labels: record.labels,
        annotations: record.annotations,
    }
}

/// The one real filtered-list computation behind `ListContainers` —
/// filters combine with AND, matching real cri-o's own
/// `filterContainerList`/`filterContainer` exactly (checked directly,
/// `server/container_list.go`): an `id` filter resolves by prefix and
/// yields an empty list (never an error) on a miss or ambiguity; when
/// both `id` and `pod_sandbox_id` are given, the resolved container's
/// own sandbox must *prefix-match* the given sandbox ID (cri-o's own
/// `strings.HasPrefix(c.Sandbox(), filter.GetPodSandboxId())`); a
/// `pod_sandbox_id` filter alone resolves the sandbox by prefix and
/// yields that sandbox's containers (or nothing for an unknown
/// sandbox); `state`/`label_selector` filter the remainder.
fn container_list_items(
    filter: Option<cri::ContainerFilter>,
) -> Result<Vec<cri::Container>, Status> {
    let root = container_store_root();

    let records = match filter.as_ref() {
        Some(f) if !f.id.is_empty() => match container::find_by_id_prefix(&root, &f.id) {
            Ok(Some(record)) => {
                if f.pod_sandbox_id.is_empty() || record.sandbox_id.starts_with(&f.pod_sandbox_id) {
                    vec![record]
                } else {
                    Vec::new()
                }
            }
            Ok(None) | Err(container::LookupError::AmbiguousPrefix(_)) => Vec::new(),
            Err(container::LookupError::Io(e)) => {
                return Err(io_error("reading container records", e));
            }
        },
        Some(f) if !f.pod_sandbox_id.is_empty() => {
            // Resolve the sandbox by prefix first, like real cri-o's
            // own `getPodSandboxFromRequest` in this exact branch --
            // an unknown sandbox is an empty list, never an error.
            match sandbox::find_by_id_prefix(&sandbox_store_root(), &f.pod_sandbox_id) {
                Ok(Some(sb)) => container::load_all(&root)
                    .map_err(|e| io_error("reading container records", e))?
                    .into_iter()
                    .filter(|r| r.sandbox_id == sb.id)
                    .collect(),
                Ok(None) | Err(sandbox::LookupError::AmbiguousPrefix(_)) => Vec::new(),
                Err(sandbox::LookupError::Io(e)) => {
                    return Err(io_error("reading sandbox records", e));
                }
            }
        }
        _ => container::load_all(&root).map_err(|e| io_error("reading container records", e))?,
    };

    // Reconcile before filtering: a state filter must see the real,
    // current state (a RUNNING record whose process already exited is
    // genuinely EXITED, whether or not anything asked about it yet).
    let records = records
        .into_iter()
        .map(reconcile_container)
        .collect::<Result<Vec<_>, _>>()?;

    Ok(records
        .into_iter()
        .filter(|record| {
            filter.as_ref().is_none_or(|f| {
                if let Some(state) = &f.state
                    && state.state != container_state_to_proto(record.state)
                {
                    return false;
                }
                f.label_selector
                    .iter()
                    .all(|(k, v)| record.labels.get(k) == Some(v))
            })
        })
        .map(container_to_proto)
        .collect())
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

    /// Creates a real, persistent pod-sandbox record with real CRI
    /// name/ID/state semantics, checked directly against real cri-o's
    /// own `runPodSandbox`/`sandboxBuilder` — and deliberately no
    /// infra ("pause") process or pinned namespaces yet (see
    /// `sandbox.rs`'s own module doc comment and `docs/design/0233`
    /// for exactly why that's real cri-o's own ordinary
    /// `drop_infra_ctr` shape too, minus the namespace pinning this
    /// project defers until it has real pod networking).
    async fn run_pod_sandbox(
        &self,
        request: Request<cri::RunPodSandboxRequest>,
    ) -> Result<Response<cri::RunPodSandboxResponse>, Status> {
        let request = request.into_inner();

        // Real cri-o validates a non-empty handler against its own
        // configured runtime table; ocicri has no configurable
        // runtime-handler concept at all (`Status` already reports
        // exactly one default handler, `name: ""`), so any non-empty
        // handler is unknown by definition -- and the proto itself
        // demands rejection for an unknown handler.
        if !request.runtime_handler.is_empty() {
            return Err(Status::invalid_argument(format!(
                "unknown runtime handler {:?}: ocicri only supports the default handler \
                 (empty string)",
                request.runtime_handler
            )));
        }

        // The same validations, in the same order, as real cri-o's own
        // `sandboxBuilder.SetConfig`/`GenerateNameAndID` (its own
        // error strings, too, where they're reasonable English).
        let config = request
            .config
            .ok_or_else(|| Status::invalid_argument("config is nil"))?;
        let metadata = config
            .metadata
            .ok_or_else(|| Status::invalid_argument("metadata is nil"))?;
        if metadata.name.is_empty() {
            return Err(Status::invalid_argument(
                "metadata.Name should not be empty",
            ));
        }
        if metadata.namespace.is_empty() {
            return Err(Status::invalid_argument(
                "cannot generate pod name without namespace",
            ));
        }
        if metadata.uid.is_empty() {
            return Err(Status::invalid_argument(
                "cannot generate pod name without uid in metadata",
            ));
        }

        // Real cri-o's own unique pod name, exactly
        // (`GenerateNameAndID`'s own strings.Join).
        let name = format!(
            "k8s_{}_{}_{}_{}",
            metadata.name, metadata.namespace, metadata.uid, metadata.attempt
        );

        let root = sandbox_store_root();
        let _guard = self
            .sandbox_mutation_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        // A duplicate request (same name/namespace/uid/attempt)
        // returns the *existing* sandbox's ID as a success -- real
        // cri-o's own `reservePodNameOrGetExisting` "this is actually
        // a duplicate request. Just return that sandbox" branch; real
        // kubelet retries after a lost response depend on this.
        if let Some(existing) =
            sandbox::find_by_name(&root, &name).map_err(|e| io_error("resolving pod name", e))?
        {
            return Ok(Response::new(cri::RunPodSandboxResponse {
                pod_sandbox_id: existing.id,
            }));
        }

        // Labels kubelet always sets but other clients (crictl)
        // don't, populated only if missing -- matching real cri-o's
        // own `populateSandboxLabels` exactly.
        let mut labels = config.labels;
        for (key, value) in [
            (POD_NAME_LABEL, &metadata.name),
            (POD_NAMESPACE_LABEL, &metadata.namespace),
            (POD_UID_LABEL, &metadata.uid),
        ] {
            labels
                .entry(key.to_string())
                .or_insert_with(|| value.clone());
        }

        // The namespace modes the request declared, stored verbatim so
        // `PodSandboxStatus` can echo them back (real cri-o's own
        // status echoes the requested options too, not a live probe).
        let namespace_options = config
            .linux
            .and_then(|l| l.security_context)
            .and_then(|sc| sc.namespace_options)
            .map(|o| sandbox::NamespaceOptions {
                network: o.network,
                pid: o.pid,
                ipc: o.ipc,
                target_id: o.target_id,
            });

        let record = sandbox::SandboxRecord {
            id: sandbox::generate_id(),
            name,
            metadata: sandbox::SandboxMetadata {
                name: metadata.name,
                uid: metadata.uid,
                namespace: metadata.namespace,
                attempt: metadata.attempt,
            },
            labels,
            annotations: config.annotations,
            state: sandbox::SandboxState::Ready,
            created_at_nanos: now_nanos(),
            namespace_options,
        };
        sandbox::save(&root, &record).map_err(|e| io_error("saving sandbox record", e))?;

        Ok(Response::new(cri::RunPodSandboxResponse {
            pod_sandbox_id: record.id,
        }))
    }

    /// `SANDBOX_READY` -> `SANDBOX_NOTREADY`, idempotently. An empty
    /// ID is a real error (real cri-o's own `sandbox.ErrIDEmpty`); an
    /// unknown ID is a silent, empty success (real cri-o's own
    /// explicit comment: "the CRI interface ... expects to not error
    /// out in not found cases").
    async fn stop_pod_sandbox(
        &self,
        request: Request<cri::StopPodSandboxRequest>,
    ) -> Result<Response<cri::StopPodSandboxResponse>, Status> {
        let id = request.into_inner().pod_sandbox_id;
        if id.is_empty() {
            // Real cri-o's own `ErrIDEmpty` message, verbatim.
            return Err(Status::invalid_argument("PodSandboxId should not be empty"));
        }

        let _guard = self
            .sandbox_mutation_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(mut record) = find_sandbox(&id)? else {
            return Ok(Response::new(cri::StopPodSandboxResponse {}));
        };
        // Idempotent for an already-stopped sandbox, matching real
        // cri-o's own `sb.Stopped()` early return.
        if record.state == sandbox::SandboxState::Ready {
            // "If there are any running containers in the sandbox,
            // they should be forcibly terminated" (the proto) --
            // real cri-o's own `stopPodSandbox` stops every container
            // first (0238).
            let container_root = container_store_root();
            for c in container::load_all(&container_root)
                .map_err(|e| io_error("reading container records", e))?
            {
                if c.sandbox_id == record.id {
                    force_kill_and_reconcile(c)?;
                }
            }
            record.state = sandbox::SandboxState::NotReady;
            sandbox::save(&sandbox_store_root(), &record)
                .map_err(|e| io_error("saving sandbox record", e))?;
        }
        Ok(Response::new(cri::StopPodSandboxResponse {}))
    }

    /// Unconditional/forceful removal (the proto: running containers
    /// "must be forcibly terminated and removed"; real cri-o's own
    /// `removePodSandbox` never requires a prior stop) -- here that
    /// means deleting the record whether `READY` or `NOTREADY`. Same
    /// empty-ID error and silent not-found success as stop.
    async fn remove_pod_sandbox(
        &self,
        request: Request<cri::RemovePodSandboxRequest>,
    ) -> Result<Response<cri::RemovePodSandboxResponse>, Status> {
        let id = request.into_inner().pod_sandbox_id;
        if id.is_empty() {
            return Err(Status::invalid_argument("PodSandboxId should not be empty"));
        }

        let _guard = self
            .sandbox_mutation_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(record) = find_sandbox(&id)? else {
            return Ok(Response::new(cri::RemovePodSandboxResponse {}));
        };
        // "If there are any containers in the sandbox, they must be
        // forcibly terminated and removed" (the proto) -- real
        // cri-o's own `removePodSandbox` deletes every container in
        // the sandbox first, and so does this (0236); a still-running
        // one is SIGKILLed first (0238).
        let container_root = container_store_root();
        for c in container::load_all(&container_root)
            .map_err(|e| io_error("reading container records", e))?
        {
            if c.sandbox_id == record.id {
                force_kill_and_reconcile(c.clone())?;
                crate::bundle::remove(&oci_cli_common::storage::default_root(), &c.id)
                    .map_err(|e| io_error("removing container bundle", e))?;
                container::remove(&container_root, &c.id)
                    .map_err(|e| io_error("removing container record", e))?;
            }
        }
        sandbox::remove(&sandbox_store_root(), &record.id)
            .map_err(|e| io_error("removing sandbox record", e))?;
        Ok(Response::new(cri::RemovePodSandboxResponse {}))
    }

    /// Unlike stop/remove, an unknown (or empty) ID here is a real
    /// gRPC `NotFound` -- real cri-o wraps every lookup failure in
    /// this RPC in `codes.NotFound` ("could not find pod %q").
    async fn pod_sandbox_status(
        &self,
        request: Request<cri::PodSandboxStatusRequest>,
    ) -> Result<Response<cri::PodSandboxStatusResponse>, Status> {
        let request = request.into_inner();
        let id = request.pod_sandbox_id;
        let Some(record) = find_sandbox(&id)? else {
            return Err(Status::not_found(format!("could not find pod {id:?}")));
        };

        // `linux.namespaces.options` echoes what the request itself
        // declared (stored verbatim at creation) -- matching real
        // cri-o, whose own status echoes `sb.NamespaceOptions()`, the
        // requested config, not a live probe.
        let linux = record
            .namespace_options
            .as_ref()
            .map(|o| cri::LinuxPodSandboxStatus {
                namespaces: Some(cri::Namespace {
                    options: Some(cri::NamespaceOption {
                        network: o.network,
                        pid: o.pid,
                        ipc: o.ipc,
                        target_id: o.target_id.clone(),
                        userns_options: None,
                    }),
                }),
            });

        // Verbose info: one "info" key holding a JSON blob, matching
        // real cri-o's own shape (`createSandboxInfo`) with honestly
        // less inside it -- there is no infra-container runtime spec
        // here to marshal, and fabricating one would be a false claim,
        // so the stored record itself is the debug payload.
        let mut info = std::collections::HashMap::new();
        if request.verbose {
            info.insert(
                "info".to_string(),
                serde_json::to_string(&record).unwrap_or_default(),
            );
        }

        Ok(Response::new(cri::PodSandboxStatusResponse {
            status: Some(cri::PodSandboxStatus {
                id: record.id.clone(),
                metadata: Some(metadata_to_proto(&record.metadata)),
                state: state_to_proto(record.state),
                created_at: record.created_at_nanos,
                // Real cri-o always sets an (empty until a CNI
                // provides an IP) network status message; ocicri has
                // no CNI at all, so an empty message is both
                // shape-identical and honest.
                network: Some(cri::PodSandboxNetworkStatus::default()),
                linux,
                labels: record.labels.clone(),
                annotations: record.annotations.clone(),
                runtime_handler: String::new(),
            }),
            info,
            // Only populated by real cri-o when its own pod-events
            // feature is enabled; ocicri has no event machinery yet.
            containers_statuses: Vec::new(),
            timestamp: 0,
        }))
    }

    /// Filters combine with AND, matching real cri-o's own
    /// `filterSandboxList`/`filterSandbox`: an `id` filter that
    /// matches nothing (or is ambiguous) yields an empty list, never
    /// an error.
    async fn list_pod_sandbox(
        &self,
        request: Request<cri::ListPodSandboxRequest>,
    ) -> Result<Response<cri::ListPodSandboxResponse>, Status> {
        let items = sandbox_list_items(request.into_inner().filter)?;
        Ok(Response::new(cri::ListPodSandboxResponse { items }))
    }

    type StreamPodSandboxesStream = BoxStream<cri::StreamPodSandboxesResponse>;

    /// The `CRIListStreaming` variant of `list_pod_sandbox`: the exact
    /// same filtered-list computation, streamed in chunks of real
    /// cri-o's own `streamChunkSize` (see `docs/design/0234` and
    /// `stream.rs`'s own module doc comment — an empty result streams
    /// zero messages and closes immediately, matching real cri-o's
    /// own `StreamPodSandboxes` exactly).
    async fn stream_pod_sandboxes(
        &self,
        request: Request<cri::StreamPodSandboxesRequest>,
    ) -> Result<Response<Self::StreamPodSandboxesStream>, Status> {
        let items = sandbox_list_items(request.into_inner().filter)?;
        Ok(Response::new(crate::stream::chunked(
            items,
            |pod_sandboxes| cri::StreamPodSandboxesResponse { pod_sandboxes },
        )))
    }

    /// Creates a real, persistent container record with real CRI
    /// name/ID/state semantics, checked directly against real cri-o's
    /// own `CreateContainer`/`container.SetConfig`/`SetNameAndID`
    /// (`server/container_create.go`, `internal/factory/container`) —
    /// and deliberately no process/bundle yet: the record is honestly
    /// `CONTAINER_CREATED`, and `StartContainer` (where the real
    /// launch machinery lands, a bigger later increment) is still a
    /// real, honest `Status::unimplemented`. See `docs/design/0236`.
    async fn create_container(
        &self,
        request: Request<cri::CreateContainerRequest>,
    ) -> Result<Response<cri::CreateContainerResponse>, Status> {
        let request = request.into_inner();

        // The same validations, in the same order, as real cri-o's
        // own `CreateContainer` preamble (its own error strings too).
        let config = request
            .config
            .ok_or_else(|| Status::invalid_argument("config is nil"))?;
        let image_spec = config
            .image
            .as_ref()
            .ok_or_else(|| Status::invalid_argument("config image is nil"))?;
        let sandbox_config = request
            .sandbox_config
            .ok_or_else(|| Status::invalid_argument("sandbox config is nil"))?;
        let pod_metadata = sandbox_config
            .metadata
            .ok_or_else(|| Status::invalid_argument("sandbox config metadata is nil"))?;

        // Sandbox lookup: an empty ID is a real error, an unknown one
        // is "specified sandbox not found" (real cri-o's own message).
        let sandbox_id = request.pod_sandbox_id;
        if sandbox_id.is_empty() {
            return Err(Status::invalid_argument("PodSandboxId should not be empty"));
        }
        let Some(sb) = find_sandbox(&sandbox_id)? else {
            return Err(Status::not_found(format!(
                "specified sandbox not found: {sandbox_id}"
            )));
        };
        // "CreateContainer failed as the sandbox was stopped" -- real
        // cri-o's own `sb.Stopped()` check, verbatim.
        if sb.state == sandbox::SandboxState::NotReady {
            return Err(Status::failed_precondition(format!(
                "CreateContainer failed as the sandbox was stopped: {}",
                sb.id
            )));
        }

        // `container.SetConfig`'s own checks (real cri-o's own error
        // strings).
        let metadata = config
            .metadata
            .ok_or_else(|| Status::invalid_argument("metadata is nil"))?;
        if metadata.name.is_empty() {
            return Err(Status::invalid_argument("name is empty"));
        }

        // The image must already be present locally -- kubelet always
        // `PullImage`s (per its own pull policy) before creating; an
        // unpulled image is a clear error, never an implicit pull
        // (there is no pull-policy input on this RPC at all).
        let image = image_spec.image.clone();
        if image.is_empty() {
            return Err(Status::invalid_argument("image not specified in config"));
        }
        let store = oci_store::Store::open(oci_cli_common::storage::default_root())
            .map_err(|e| Status::internal(format!("opening image storage: {e}")))?;
        let Some(resolved) = oci_store::resolve_by_reference_or_id(&store, &image)
            .map_err(|e| Status::internal(format!("resolving image: {e}")))?
        else {
            return Err(Status::not_found(format!(
                "image {image:?} not present locally: pull it first (PullImage)"
            )));
        };
        let image_ref = resolved.record().manifest_digest.to_string();

        // Real cri-o's own unique container name, exactly
        // (`SetNameAndID`'s own strings.Join -- the pod half comes
        // from the *request's* own sandbox_config, matching cri-o).
        let name = format!(
            "k8s_{}_{}_{}_{}_{}",
            metadata.name,
            pod_metadata.name,
            pod_metadata.namespace,
            pod_metadata.uid,
            metadata.attempt
        );

        let root = container_store_root();
        let _guard = self
            .sandbox_mutation_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        // A duplicate request returns the *existing* container's ID
        // as a success -- real cri-o's own "this is actually a
        // duplicate request. Just return that container" branch.
        if let Some(existing) = container::find_by_name(&root, &name)
            .map_err(|e| io_error("resolving container name", e))?
        {
            return Ok(Response::new(cri::CreateContainerResponse {
                container_id: existing.id,
            }));
        }

        let container_id = crate::records::generate_id();

        // Prepare the real, launch-ready bundle (dedicated writable
        // rootfs + generated, validation-round-tripped config.json)
        // before ever recording the container -- matching real
        // cri-o's own create-time storage/spec preparation, and
        // guaranteeing a recorded container always has its bundle
        // (`docs/design/0237`). `prepare` cleans up after itself on
        // failure, so a rejected create leaves nothing behind.
        let manifest = store
            .image_manifest(resolved.record())
            .map_err(|e| Status::internal(format!("reading image manifest: {e}")))?;
        let image_config = store
            .image_config(resolved.record())
            .map_err(|e| Status::internal(format!("reading image config: {e}")))?
            .config
            .unwrap_or_default();
        let envs: Vec<String> = config
            .envs
            .iter()
            .map(|kv| format!("{}={}", kv.key, kv.value))
            .collect();
        crate::bundle::prepare(
            &store,
            &oci_cli_common::storage::default_root(),
            &container_id,
            &manifest,
            &image_config,
            &crate::bundle::CriProcessConfig {
                command: &config.command,
                args: &config.args,
                envs,
                working_dir: &config.working_dir,
            },
        )
        .map_err(|e| match e {
            // Real cri-o's own verbatim error for a container with
            // nothing to run at all -- a client-input problem.
            crate::bundle::PrepareError::NoCommand => {
                Status::invalid_argument("no command specified")
            }
            crate::bundle::PrepareError::Other(e) => {
                Status::internal(format!("preparing container bundle: {e:#}"))
            }
        })?;

        let record = container::ContainerRecord {
            id: container_id,
            name,
            sandbox_id: sb.id,
            metadata: container::ContainerMetadata {
                name: metadata.name,
                attempt: metadata.attempt,
            },
            image,
            image_ref,
            labels: config.labels,
            annotations: config.annotations,
            state: container::ContainerState::Created,
            created_at_nanos: now_nanos(),
            pid: None,
            started_at_nanos: None,
            finished_at_nanos: None,
            exit_code: None,
        };
        if let Err(e) = container::save(&root, &record) {
            // Never leave an orphaned bundle behind a failed record
            // write (the record is what makes the bundle reachable).
            let _ = crate::bundle::remove(&oci_cli_common::storage::default_root(), &record.id);
            return Err(io_error("saving container record", e));
        }

        Ok(Response::new(cri::CreateContainerResponse {
            container_id: record.id,
        }))
    }

    /// Actually starts the container (`docs/design/0238`): spawns the
    /// per-container launcher-keeper (`launcher.rs`, this project's
    /// own conmon equivalent — a fresh, single-threaded re-exec of
    /// this same binary, since `oci_runtime_core::launch`'s
    /// fork-safety contract is unsatisfiable from a tokio server),
    /// waits for the real pid, and records `RUNNING`. Only a
    /// `CONTAINER_CREATED` container can be started — real cri-o's
    /// own verbatim "is not in created state" error otherwise; an
    /// unknown ID is a real `NotFound` (its `container_start.go`).
    async fn start_container(
        &self,
        request: Request<cri::StartContainerRequest>,
    ) -> Result<Response<cri::StartContainerResponse>, Status> {
        let id = request.into_inner().container_id;
        let record = {
            let _guard = self
                .sandbox_mutation_lock
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(record) = find_container(&id)? else {
                return Err(Status::not_found(format!(
                    "could not find container {id:?}"
                )));
            };
            let record = reconcile_container(record)?;
            if record.state != container::ContainerState::Created {
                return Err(Status::failed_precondition(format!(
                    "container {} is not in created state: {:?}",
                    record.id, record.state
                )));
            }
            record
        };

        let bundle_dir =
            crate::bundle::bundle_dir(&oci_cli_common::storage::default_root(), &record.id);

        // Spawn the launcher-keeper: a fresh re-exec of this binary
        // (fork+immediate-exec, safe from a multithreaded parent).
        // Null stdio: the launcher's own failure reporting goes
        // through its `start-error` file, never a pipe this server
        // would have to babysit.
        let exe = std::env::current_exe()
            .map_err(|e| Status::internal(format!("resolving own executable: {e}")))?;
        let mut child = std::process::Command::new(exe)
            .arg(crate::launcher::LAUNCH_ARGV1)
            .arg(&bundle_dir)
            .arg(&record.id)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| Status::internal(format!("spawning container launcher: {e}")))?;

        // Reap the launcher whenever it eventually exits (its own
        // lifetime is the container's, not this RPC's) so it never
        // lingers as a zombie child of this long-lived server.
        std::thread::spawn(move || {
            let _ = child.wait();
        });

        // Wait (bounded) for the launcher to report the real pid --
        // or a real start failure. Async sleeps: never park a tokio
        // worker thread.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let pid = loop {
            if let Some(pid) = crate::launcher::read_pid(&bundle_dir) {
                break pid;
            }
            if let Some(reason) = crate::launcher::read_start_error(&bundle_dir) {
                return Err(Status::internal(format!(
                    "starting container {}: {reason}",
                    record.id
                )));
            }
            if std::time::Instant::now() >= deadline {
                return Err(Status::internal(format!(
                    "starting container {}: launcher reported neither a pid nor an error in time",
                    record.id
                )));
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        };

        {
            let _guard = self
                .sandbox_mutation_lock
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut record = record;
            record.state = container::ContainerState::Running;
            record.pid = Some(pid);
            record.started_at_nanos = Some(now_nanos());
            container::save(&container_store_root(), &record)
                .map_err(|e| io_error("saving container record", e))?;
        }
        Ok(Response::new(cri::StartContainerResponse {}))
    }

    /// Real cri-o's own stop semantics, checked directly
    /// (`server/container_stop.go`, `internal/oci/runtime_oci.go`):
    /// unknown ID is a silent, idempotent success ("must not return
    /// an error if the container has already been stopped");
    /// a container with no living process (never started, or already
    /// exited) just gets its finished state settled; a running one
    /// gets the stop signal (SIGTERM — per-image `STOPSIGNAL` is a
    /// documented later increment), `timeout` seconds to comply, then
    /// SIGKILL.
    async fn stop_container(
        &self,
        request: Request<cri::StopContainerRequest>,
    ) -> Result<Response<cri::StopContainerResponse>, Status> {
        let request = request.into_inner();
        let id = request.container_id;
        if id.is_empty() {
            return Err(Status::invalid_argument("ContainerId should not be empty"));
        }

        let record = {
            let _guard = self
                .sandbox_mutation_lock
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(record) = find_container(&id)? else {
                return Ok(Response::new(cri::StopContainerResponse {}));
            };
            let mut record = reconcile_container(record)?;
            match record.state {
                container::ContainerState::Exited => {
                    return Ok(Response::new(cri::StopContainerResponse {}));
                }
                container::ContainerState::Created => {
                    // No process ever existed -- settle the state,
                    // matching real cri-o's own `Living()`-fails path
                    // (`c.state.Finished = time.Now()`, no exit code).
                    record.state = container::ContainerState::Exited;
                    record.finished_at_nanos = Some(now_nanos());
                    container::save(&container_store_root(), &record)
                        .map_err(|e| io_error("saving container record", e))?;
                    return Ok(Response::new(cri::StopContainerResponse {}));
                }
                container::ContainerState::Running => record,
            }
        };

        let bundle_dir =
            crate::bundle::bundle_dir(&oci_cli_common::storage::default_root(), &record.id);
        let pid = record.pid;

        // Grace period first (only if the caller granted one): the
        // stop signal, then up to `timeout` seconds for a voluntary
        // exit.
        if let (Some(pid), true) = (pid, request.timeout > 0) {
            let _ = oci_runtime_core::process::kill(pid, libc::SIGTERM);
            let deadline = std::time::Instant::now()
                + std::time::Duration::from_secs(request.timeout.min(600) as u64);
            while std::time::Instant::now() < deadline {
                if crate::launcher::read_exit(&bundle_dir)
                    .map_err(|e| Status::internal(format!("reading exit record: {e}")))?
                    .is_some()
                    || !pid_alive(pid)
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }

        // Forceful half (a no-op if the grace period already worked):
        // SIGKILL, then settle the record from the launcher's own
        // exit file. Runs on the blocking pool -- `force_kill_and_
        // reconcile` polls with real sleeps.
        let settled = tokio::task::spawn_blocking(move || force_kill_and_reconcile(record))
            .await
            .map_err(|e| Status::internal(format!("stop task panicked: {e}")))??;
        {
            let _guard = self
                .sandbox_mutation_lock
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            container::save(&container_store_root(), &settled)
                .map_err(|e| io_error("saving container record", e))?;
        }
        Ok(Response::new(cri::StopContainerResponse {}))
    }

    /// Idempotent, forceful removal — the proto: "must not return an
    /// error if the container has already been removed", matched by
    /// real cri-o's own `truncindex.ErrNotExist -> empty response`
    /// branch (`server/container_remove.go`, checked directly). No
    /// prior stop is ever required. An empty ID is a real error, the
    /// same rule the sandbox RPCs already apply.
    async fn remove_container(
        &self,
        request: Request<cri::RemoveContainerRequest>,
    ) -> Result<Response<cri::RemoveContainerResponse>, Status> {
        let id = request.into_inner().container_id;
        if id.is_empty() {
            return Err(Status::invalid_argument("ContainerId should not be empty"));
        }

        let record = {
            let _guard = self
                .sandbox_mutation_lock
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(record) = find_container(&id)? else {
                return Ok(Response::new(cri::RemoveContainerResponse {}));
            };
            record
        };
        // Forceful: a still-running container is SIGKILLed first (the
        // proto's own contract), on the blocking pool (the kill wait
        // polls with real sleeps).
        tokio::task::spawn_blocking(move || force_kill_and_reconcile(record))
            .await
            .map_err(|e| Status::internal(format!("remove task panicked: {e}")))??;

        let _guard = self
            .sandbox_mutation_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Re-resolve under the lock: the settle above ran unlocked.
        let Some(record) = find_container(&id)? else {
            return Ok(Response::new(cri::RemoveContainerResponse {}));
        };
        crate::bundle::remove(&oci_cli_common::storage::default_root(), &record.id)
            .map_err(|e| io_error("removing container bundle", e))?;
        container::remove(&container_store_root(), &record.id)
            .map_err(|e| io_error("removing container record", e))?;
        Ok(Response::new(cri::RemoveContainerResponse {}))
    }

    /// Filters combine with AND, matching real cri-o's own
    /// `filterContainerList`/`filterContainer` — see
    /// [`container_list_items`]'s own doc comment for each rule's
    /// exact real-cri-o citation.
    async fn list_containers(
        &self,
        request: Request<cri::ListContainersRequest>,
    ) -> Result<Response<cri::ListContainersResponse>, Status> {
        let containers = container_list_items(request.into_inner().filter)?;
        Ok(Response::new(cri::ListContainersResponse { containers }))
    }

    type StreamContainersStream = BoxStream<cri::StreamContainersResponse>;

    async fn stream_containers(
        &self,
        _request: Request<cri::StreamContainersRequest>,
    ) -> Result<Response<Self::StreamContainersStream>, Status> {
        unimplemented("StreamContainers")
    }

    /// An unknown (or empty) ID is a real gRPC `NotFound` — real
    /// cri-o wraps every lookup failure here in `codes.NotFound`
    /// ("could not find container %q", `server/container_status.go`),
    /// the same asymmetry-with-remove the sandbox RPCs already
    /// mirror. Every record this slice can produce is honestly
    /// `CONTAINER_CREATED`, so no `started_at`/`finished_at`/
    /// `exit_code` is ever reported (real cri-o sets those only for
    /// the running/stopped states this slice can't reach yet).
    async fn container_status(
        &self,
        request: Request<cri::ContainerStatusRequest>,
    ) -> Result<Response<cri::ContainerStatusResponse>, Status> {
        let request = request.into_inner();
        let id = request.container_id;
        let record = {
            let _guard = self
                .sandbox_mutation_lock
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(record) = find_container(&id)? else {
                return Err(Status::not_found(format!(
                    "could not find container {id:?}"
                )));
            };
            reconcile_container(record)?
        };

        // Verbose info: one "info" key holding a JSON blob, the same
        // shape (and the same honestly-smaller payload) the sandbox
        // status RPC already established -- there is no runtime
        // spec/pid here to marshal until StartContainer exists.
        let mut info = std::collections::HashMap::new();
        if request.verbose {
            info.insert(
                "info".to_string(),
                serde_json::to_string(&record).unwrap_or_default(),
            );
        }

        // Exit reporting for an EXITED container, matching real
        // cri-o's own status switch (`container_status.go`): a real
        // recorded exit code (or its own identical `-1` fallback when
        // none was ever recorded), and the kubelet-conventional
        // `Completed`/`Error` reason real cri-o's own containers
        // report.
        let (exit_code, reason) = match record.state {
            container::ContainerState::Exited => match record.exit_code {
                Some(0) => (0, "Completed".to_string()),
                Some(code) => (code, "Error".to_string()),
                None => (-1, "Error".to_string()),
            },
            _ => (0, String::new()),
        };

        Ok(Response::new(cri::ContainerStatusResponse {
            status: Some(cri::ContainerStatus {
                id: record.id.clone(),
                metadata: Some(container_metadata_to_proto(&record.metadata)),
                state: container_state_to_proto(record.state),
                created_at: record.created_at_nanos,
                started_at: record.started_at_nanos.unwrap_or(0),
                finished_at: record.finished_at_nanos.unwrap_or(0),
                exit_code,
                reason,
                image: Some(cri::ImageSpec {
                    image: record.image.clone(),
                    ..Default::default()
                }),
                image_ref: record.image_ref.clone(),
                image_id: record.image_ref.clone(),
                labels: record.labels.clone(),
                annotations: record.annotations.clone(),
                ..Default::default()
            }),
            info,
        }))
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

    /// A real, unconditional no-op — matching real `cri-o`'s own
    /// identical implementation exactly (`server/update_runtime_
    /// config.go`, checked directly: it doesn't even read the request
    /// body, just returns an empty response). This RPC exists to push
    /// a kubelet-allocated pod CIDR into the runtime for the old
    /// *kubenet* network plugin era; kubenet was removed from
    /// Kubernetes years ago, and modern CNI plugins get their own IP
    /// allocation through their own IPAM, never through this RPC — so
    /// silently discarding the given `pod_cidr` is genuinely the
    /// correct, current behavior, not a shortcut around anything this
    /// project doesn't support yet (real `cri-o` reaches the exact
    /// same conclusion, on a codebase with every real networking
    /// capability this project's own `ocicri` doesn't have).
    async fn update_runtime_config(
        &self,
        _request: Request<cri::UpdateRuntimeConfigRequest>,
    ) -> Result<Response<cri::UpdateRuntimeConfigResponse>, Status> {
        Ok(Response::new(cri::UpdateRuntimeConfigResponse {}))
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

    /// A real, honest empty list — checked directly against real
    /// `cri-o`'s own implementation (`server/metric_descriptors_
    /// list.go`): its own descriptor table (`internal/lib/
    /// statsserver/descriptors.go`) is entirely static/config-driven
    /// (never touches any real container/sandbox state), gated by
    /// `crio.conf`'s own `included_pod_metrics` — which *defaults to
    /// empty*, so a real, unconfigured `cri-o` install already
    /// answers with almost nothing (one always-on descriptor,
    /// `container_last_seen`). `ocicri` has no metrics collection
    /// machinery of its own at all yet — no RPC in `ImageService`/
    /// `RuntimeService` populates any real per-container metric value
    /// anywhere (`ListPodSandboxMetrics`/`StreamPodSandboxMetrics`
    /// remain real, honest `Status::unimplemented`s below) — so
    /// advertising even that one always-on descriptor here would be a
    /// real, false claim: a caller could reasonably expect a
    /// following `ListPodSandboxMetrics` call to actually return a
    /// value for whatever this RPC just told it exists. An empty list
    /// is genuinely the most honest possible answer, not a
    /// placeholder — real cri-o's own architecture already
    /// establishes that returning nothing here is a normal, valid,
    /// unconfigured-install response, not an error condition kubelet
    /// needs to special-case.
    async fn list_metric_descriptors(
        &self,
        _request: Request<cri::ListMetricDescriptorsRequest>,
    ) -> Result<Response<cri::ListMetricDescriptorsResponse>, Status> {
        Ok(Response::new(cri::ListMetricDescriptorsResponse {
            descriptors: Vec::new(),
        }))
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

    /// Reports the real cgroup driver this project's own container-
    /// orchestration binary (`ociman run`/`create`) actually,
    /// unconditionally uses today: the **systemd** driver (a real
    /// transient scope via `oci_runtime_core::systemd_cgroup`,
    /// checked directly in `bin/ociman/src/main.rs` — `ociman` never
    /// falls through to plain cgroupfs at all, regardless of the
    /// spec's own `cgroupsPath`). `ocirun run` itself uses plain
    /// cgroupfs instead (`CgroupSetup::FromSpec`, matching real
    /// `runc`/`crun`'s own spec-driven behavior) — but `ocirun` is the
    /// low-level OCI runtime layer, not what a real kubelet is asking
    /// about here; the CRI-facing answer is about this project's own
    /// container-orchestration behavior, the same one `ociman`
    /// already establishes. This also matches real `cri-o`'s own
    /// checked-directly default (`internal/config/cgmgr/
    /// cgmgr_linux.go`'s own `DefaultCgroupManager = systemd`,
    /// confirmed by `crio.conf`'s own shipped default) — not a
    /// coincidence: both this project and real `cri-o` land on
    /// systemd as the sane default for a real systemd-based host.
    async fn runtime_config(
        &self,
        _request: Request<cri::RuntimeConfigRequest>,
    ) -> Result<Response<cri::RuntimeConfigResponse>, Status> {
        Ok(Response::new(cri::RuntimeConfigResponse {
            linux: Some(cri::LinuxRuntimeConfiguration {
                cgroup_driver: cri::CgroupDriver::Systemd as i32,
            }),
        }))
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
        let service = RuntimeServiceImpl::default();
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
        let service = RuntimeServiceImpl::default();
        let status = service
            .port_forward(Request::new(cri::PortForwardRequest {
                pod_sandbox_id: String::new(),
                port: Vec::new(),
            }))
            .await
            .unwrap_err();
        assert_eq!(status.code(), tonic::Code::Unimplemented);
        assert!(status.message().contains("PortForward"), "{status:?}");
    }

    #[tokio::test]
    async fn create_container_with_no_config_at_all_is_invalid_argument() {
        let service = RuntimeServiceImpl::default();
        let status = service
            .create_container(Request::new(cri::CreateContainerRequest {
                pod_sandbox_id: String::new(),
                config: None,
                sandbox_config: None,
            }))
            .await
            .unwrap_err();
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert!(status.message().contains("config is nil"), "{status:?}");
    }

    #[tokio::test]
    async fn remove_container_with_an_empty_id_is_a_real_error() {
        let service = RuntimeServiceImpl::default();
        let status = service
            .remove_container(Request::new(cri::RemoveContainerRequest {
                container_id: String::new(),
            }))
            .await
            .unwrap_err();
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert!(
            status.message().contains("ContainerId should not be empty"),
            "{status:?}"
        );
    }

    // `run_pod_sandbox`'s own real create/duplicate/stop/remove/
    // status/list lifecycle cases are covered by the real, socket-
    // connecting integration tests in `tests/tests/ocicri_pod_
    // sandbox.rs` instead of here: the sandbox store reads the real
    // process-global `OCI_TOOLS_STORAGE_ROOT` environment variable
    // directly (the same reasoning `image_service.rs`'s own tests
    // already document) -- the request-shape validations below need
    // no store access at all, so they're safe here.

    #[tokio::test]
    async fn run_pod_sandbox_with_no_config_at_all_is_invalid_argument() {
        let service = RuntimeServiceImpl::default();
        let status = service
            .run_pod_sandbox(Request::new(cri::RunPodSandboxRequest {
                config: None,
                runtime_handler: String::new(),
            }))
            .await
            .unwrap_err();
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert!(status.message().contains("config is nil"), "{status:?}");
    }

    #[tokio::test]
    async fn run_pod_sandbox_with_a_nonempty_runtime_handler_is_rejected() {
        let service = RuntimeServiceImpl::default();
        let status = service
            .run_pod_sandbox(Request::new(cri::RunPodSandboxRequest {
                config: None,
                runtime_handler: "kata".to_string(),
            }))
            .await
            .unwrap_err();
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert!(
            status.message().contains("unknown runtime handler"),
            "{status:?}"
        );
    }

    #[tokio::test]
    async fn stop_and_remove_with_an_empty_id_are_real_errors() {
        let service = RuntimeServiceImpl::default();
        let status = service
            .stop_pod_sandbox(Request::new(cri::StopPodSandboxRequest {
                pod_sandbox_id: String::new(),
            }))
            .await
            .unwrap_err();
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        // Real cri-o's own `ErrIDEmpty` message, verbatim.
        assert!(
            status
                .message()
                .contains("PodSandboxId should not be empty"),
            "{status:?}"
        );

        let status = service
            .remove_pod_sandbox(Request::new(cri::RemovePodSandboxRequest {
                pod_sandbox_id: String::new(),
            }))
            .await
            .unwrap_err();
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
    }
}
