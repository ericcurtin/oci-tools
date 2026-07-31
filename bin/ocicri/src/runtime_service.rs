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
         UpdateRuntimeConfig/ListMetricDescriptors, the pod-sandbox lifecycle, the container \
         lifecycle (create/start/stop/remove/status/list), ExecSync, container stats, and \
         CRI log files including ReopenContainerLog are answered so far)"
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

/// The one real filtered computation behind `PodSandboxStats`/
/// `ListPodSandboxStats`/`StreamPodSandboxStats` (`docs/design/0262`)
/// — the exact same `id`/`label_selector`-resolution shape
/// `sandbox_list_items` already uses (its own `state` field simply
/// doesn't exist on `PodSandboxStatsFilter`, matching the real proto's
/// own narrower filter message), the same "reuse the list resolution
/// by mapping the stats filter onto the list filter's own identical
/// fields" pattern `container_stats_items` already established for
/// its own container-level sibling.
///
/// `Linux` is always `None`: real cri-o's own `PodSandboxStats`
/// reports live cgroup/network numbers from the sandbox's own infra
/// ("pause") container's cgroup — this project deliberately has no
/// infra process or per-sandbox cgroup of its own at all
/// (`docs/design/0233`), so there is honestly nothing to report there,
/// the same "absence over fabrication" rule `ContainerStats`
/// (`docs/design/0241`) already applies for a container with no live
/// cgroup. `Attributes` (id/metadata/labels/annotations) *are* real
/// and always available from the sandbox record regardless, so they're
/// reported in full rather than omitting the whole response.
fn pod_sandbox_stats_items(
    filter: Option<cri::PodSandboxStatsFilter>,
) -> Result<Vec<cri::PodSandboxStats>, Status> {
    let list_filter = filter.map(|f| cri::PodSandboxFilter {
        id: f.id,
        label_selector: f.label_selector,
        state: None,
    });
    let records = match list_filter.as_ref().map(|f| f.id.as_str()) {
        Some(id) if !id.is_empty() => match sandbox::find_by_id_prefix(&sandbox_store_root(), id) {
            Ok(Some(record)) => vec![record],
            Ok(None) | Err(sandbox::LookupError::AmbiguousPrefix(_)) => Vec::new(),
            Err(sandbox::LookupError::Io(e)) => {
                return Err(io_error("reading sandbox records", e));
            }
        },
        _ => sandbox::load_all(&sandbox_store_root())
            .map_err(|e| io_error("reading sandbox records", e))?,
    };

    Ok(records
        .into_iter()
        .filter(|record| {
            list_filter
                .as_ref()
                .is_none_or(|filter| matches_filter(record, filter))
        })
        .map(|record| cri::PodSandboxStats {
            attributes: Some(cri::PodSandboxAttributes {
                id: record.id.clone(),
                metadata: Some(metadata_to_proto(&record.metadata)),
                labels: record.labels,
                annotations: record.annotations,
            }),
            linux: None,
            windows: None,
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

/// Builds one container's real CRI stats (`docs/design/0241`) from
/// its live cgroup and its bundle rootfs — the same shared
/// `oci_runtime_core::cgroups` readers `ociman stats` already uses,
/// mapped onto the CRI message shapes with real cri-o's own cgroup-v2
/// formulas (`internal/lib/statsserver/stats_server_linux.go`,
/// checked directly): `usage_core_nano_seconds` from `cpu.stat`,
/// `working_set_bytes` = current − `inactive_file` (the shared
/// `memory_usage_bytes` already computes exactly this), `usage_bytes`
/// raw, `rss = anon`, `pgfault`/`pgmajfault`, `available_bytes` only
/// when a real limit exists, `pids.current`, and the writable layer's
/// own real disk usage (the bundle rootfs, via the same
/// hardlink-aware `oci_store::dir_stats` `ImageFsInfo` uses).
///
/// `None` for anything without a live, readable cgroup: a
/// created/exited container, a pid that died mid-read, or a launch
/// whose systemd-scope setup fell back to no cgroup at all (the
/// documented rootless no-D-Bus fallback) — matching real cri-o,
/// whose own stats server likewise only reports containers with live
/// cgroup accounting. `usage_nano_cores` is deliberately never
/// fabricated: real cri-o derives it from *two* samples over time
/// (its own `updateUsageNanoCores` cache); a single-shot reader has
/// no honest value to put there, and kubelet computes rates from
/// `usage_core_nano_seconds` itself.
fn container_stats_for(record: &container::ContainerRecord) -> Option<cri::ContainerStats> {
    if record.state != container::ContainerState::Running {
        return None;
    }
    let pid = record.pid?;
    let cgroup_dir = oci_runtime_core::cgroups::cgroup_dir_for_running_pid(
        std::path::Path::new("/sys/fs/cgroup"),
        pid,
    )
    .ok()?;

    let now = now_nanos();
    let cpu_nanos = oci_runtime_core::cgroups::cpu_usage_nanos(&cgroup_dir).ok()?;
    let working_set = oci_runtime_core::cgroups::memory_usage_bytes(&cgroup_dir).ok()?;
    let usage_bytes = oci_runtime_core::cgroups::memory_current_bytes(&cgroup_dir).ok()?;
    let rss = oci_runtime_core::cgroups::memory_stat_key(&cgroup_dir, "anon").unwrap_or(0);
    let page_faults =
        oci_runtime_core::cgroups::memory_stat_key(&cgroup_dir, "pgfault").unwrap_or(0);
    let major_page_faults =
        oci_runtime_core::cgroups::memory_stat_key(&cgroup_dir, "pgmajfault").unwrap_or(0);
    let limit = oci_runtime_core::cgroups::memory_limit_bytes(&cgroup_dir).unwrap_or(u64::MAX);
    let available = (limit != u64::MAX).then(|| limit.saturating_sub(working_set));
    // (`pids.current` belongs to the CRI's *sandbox* stats message
    // (`ProcessUsage`), which stays unimplemented -- the container
    // stats message has no process field at all.)

    let rootfs = crate::bundle::bundle_dir(&oci_cli_common::storage::default_root(), &record.id)
        .join("rootfs");
    let writable_layer =
        oci_store::dir_stats(&rootfs)
            .ok()
            .map(|(bytes, files)| cri::FilesystemUsage {
                timestamp: now,
                fs_id: Some(cri::FilesystemIdentifier {
                    mountpoint: rootfs.display().to_string(),
                }),
                used_bytes: Some(cri::UInt64Value { value: bytes }),
                inodes_used: Some(cri::UInt64Value { value: files }),
            });

    Some(cri::ContainerStats {
        attributes: Some(cri::ContainerAttributes {
            id: record.id.clone(),
            metadata: Some(container_metadata_to_proto(&record.metadata)),
            labels: record.labels.clone(),
            annotations: record.annotations.clone(),
        }),
        cpu: Some(cri::CpuUsage {
            timestamp: now,
            usage_core_nano_seconds: Some(cri::UInt64Value { value: cpu_nanos }),
            usage_nano_cores: None,
            // PSI accounting: a real, optional kernel feature real
            // cri-o itself only reports when available -- not read
            // here yet (docs/design/0241).
            psi: None,
        }),
        memory: Some(cri::MemoryUsage {
            timestamp: now,
            working_set_bytes: Some(cri::UInt64Value { value: working_set }),
            usage_bytes: Some(cri::UInt64Value { value: usage_bytes }),
            rss_bytes: Some(cri::UInt64Value { value: rss }),
            page_faults: Some(cri::UInt64Value { value: page_faults }),
            major_page_faults: Some(cri::UInt64Value {
                value: major_page_faults,
            }),
            available_bytes: available.map(|value| cri::UInt64Value { value }),
            psi: None,
        }),
        writable_layer,
        swap: None,
        io: None,
    })
}

/// The filtered stats-list computation behind both
/// `ListContainerStats` and its `CRIListStreaming` sibling — the
/// filter is `ContainerStatsFilter` (`id`/`pod_sandbox_id`/
/// `label_selector`, no state field), with the same AND/prefix rules
/// as [`container_list_items`]; containers without live cgroup
/// accounting are silently absent, matching real cri-o's own stats
/// server.
fn container_stats_items(
    filter: Option<cri::ContainerStatsFilter>,
) -> Result<Vec<cri::ContainerStats>, Status> {
    // Reuse the container-list resolution by mapping the stats filter
    // onto the list filter's own identical id/sandbox/label fields.
    let list_filter = filter.map(|f| cri::ContainerFilter {
        id: f.id,
        pod_sandbox_id: f.pod_sandbox_id,
        label_selector: f.label_selector,
        state: None,
    });
    let root = container_store_root();
    let records = match list_filter.as_ref() {
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

    Ok(records
        .into_iter()
        .map(reconcile_container)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|record| {
            list_filter.as_ref().is_none_or(|f| {
                f.label_selector
                    .iter()
                    .all(|(k, v)| record.labels.get(k) == Some(v))
            })
        })
        .filter_map(|record| container_stats_for(&record))
        .collect())
}

/// What one `ExecSync` helper run produced — the blocking half's own
/// result shape (see `exec_sync`).
struct ExecOutcome {
    timed_out: bool,
    exit_code: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn container_state_to_proto(state: container::ContainerState) -> i32 {
    match state {
        container::ContainerState::Created => cri::ContainerState::ContainerCreated as i32,
        container::ContainerState::Running => cri::ContainerState::ContainerRunning as i32,
        container::ContainerState::Exited => cri::ContainerState::ContainerExited as i32,
    }
}

/// Converts a CRI `LinuxContainerResources` request into the same
/// [`oci_spec_types::runtime::LinuxResources`] shape `ociman update`/
/// `ocirun update` already apply — see `update_container_resources`'s
/// own doc comment for exactly which fields map where (and which
/// three genuinely have no home yet). `0`/`""` throughout is the
/// proto's own documented "not specified" sentinel, so every field
/// maps to `None`/omitted rather than a real, if degenerate, `0`
/// value -- a caller that actually wants e.g. `cpu_shares: 0` has no
/// way to ask for that over this RPC either way (the proto gives it
/// no other meaning), so this loses nothing real.
fn linux_container_resources_to_oci(
    linux: &cri::LinuxContainerResources,
) -> oci_spec_types::runtime::LinuxResources {
    let memory = oci_spec_types::runtime::LinuxMemory {
        limit: (linux.memory_limit_in_bytes != 0).then_some(linux.memory_limit_in_bytes),
        swap: (linux.memory_swap_limit_in_bytes != 0).then_some(linux.memory_swap_limit_in_bytes),
        ..Default::default()
    };
    let cpu = oci_spec_types::runtime::LinuxCpu {
        shares: (linux.cpu_shares != 0).then_some(linux.cpu_shares as u64),
        period: (linux.cpu_period != 0).then_some(linux.cpu_period as u64),
        quota: (linux.cpu_quota != 0).then_some(linux.cpu_quota),
        cpus: linux.cpuset_cpus.clone(),
        mems: linux.cpuset_mems.clone(),
        ..Default::default()
    };
    oci_spec_types::runtime::LinuxResources {
        memory: Some(memory),
        cpu: Some(cpu),
        ..Default::default()
    }
}

fn container_metadata_to_proto(metadata: &container::ContainerMetadata) -> cri::ContainerMetadata {
    cri::ContainerMetadata {
        name: metadata.name.clone(),
        attempt: metadata.attempt,
    }
}

/// `ContainerConfig.linux.security_context`'s own `run_as_user`/
/// `run_as_group`/`run_as_username` (0365, closing part of `0237`'s
/// own deferred "per-container run_as_user/security-context mapping"
/// gap): a container this project creates is always rootless with
/// exactly one real uid/gid mapped -- container `0`, to this
/// process's own euid/egid (`Spec::into_rootless`'s own single-entry
/// mapping) -- the exact same constraint `ociman run --user`'s own
/// `resolve_user` already established and gives a clear, immediate
/// error for instead of a confusing, much-later kernel `EINVAL`. This
/// closes the identical real gap for `ocicri`: before this, a pod's
/// own explicit `securityContext.runAsUser: 1000` was silently
/// ignored entirely, and the container quietly ran as uid `0` anyway
/// -- a real, previously-undetected divergence from the pod spec's
/// own explicit intent, now a clear, loud error instead of a silent
/// no-op. `run_as_user: 0`/`run_as_group: 0` (a real, common,
/// legitimate request many pods make explicitly) already matches
/// this project's own existing default, so it needs no new spec
/// field at all -- only this validation was ever missing.
///
/// `run_as_username` (name-resolved against the image's own real
/// `/etc/passwd`, matching real cri-o's own `GetUserInfo`) is
/// deliberately not supported at all yet, the same "numeric only, a
/// higher-level-tool concern" scope `ocirun exec --user`'s own doc
/// comment already established -- a clear `Status::unimplemented`
/// rather than silently ignored too. `run_as_group` given without
/// `run_as_user`/`run_as_username` is a real, immediate error,
/// reusing real cri-o's own exact message verbatim (checked directly,
/// `~/git/cri-o/server/container_create.go`'s own `setupContainerUser`).
///
/// Still deliberately out of scope: an image's own declared `USER`
/// (real cri-o's own `imageUser` fallback) is never read or applied
/// here either -- a separate, pre-existing gap this note doesn't
/// close, unrelated to the CRI-requested `run_as_user`/`run_as_group`
/// this note is actually about.
fn validate_run_as_user(
    security_context: Option<&cri::LinuxContainerSecurityContext>,
) -> Result<(), Status> {
    let Some(sc) = security_context else {
        return Ok(());
    };
    if !sc.run_as_username.is_empty() {
        return Err(Status::unimplemented(
            "run_as_username is not yet supported (numeric run_as_user/run_as_group only)",
        ));
    }
    if sc.run_as_group.is_some() && sc.run_as_user.is_none() {
        // Real cri-o's own exact message.
        return Err(Status::invalid_argument(
            "user group is specified without user or username",
        ));
    }
    if let Some(uid) = sc.run_as_user.as_ref()
        && uid.value != 0
    {
        return Err(Status::unimplemented(format!(
            "run_as_user {} resolves to a non-root container uid, which this rootless \
             runtime cannot map yet (only container uid 0 is mapped, to this process's own \
             euid; a subordinate uid range via /etc/subuid would be needed for anything else)",
            uid.value
        )));
    }
    if let Some(gid) = sc.run_as_group.as_ref()
        && gid.value != 0
    {
        return Err(Status::unimplemented(format!(
            "run_as_group {} resolves to a non-root container gid, which this rootless \
             runtime cannot map yet (only container gid 0 is mapped, to this process's own \
             egid; a subordinate gid range via /etc/subgid would be needed for anything else)",
            gid.value
        )));
    }
    Ok(())
}

/// `ContainerConfig.linux.security_context.privileged` (0389): unlike
/// every other field this function's own sibling (`validate_run_as_
/// user`) already checks, `privileged` was previously read *nowhere
/// at all* -- not honored, not rejected, a real, silent divergence
/// from a pod's own explicit intent (worse than a merely-unsupported
/// field: a workload asking for privileged access got an ordinary,
/// confined container instead, with no error telling it so).
///
/// Real cri-o's own `privileged` support is a large, many-part
/// feature (checked directly, `~/git/cri-o/server/container_create_
/// linux.go`'s own `getSpecGen`/`specSetDevices`/`addSysfsMounts` and
/// friends): every capability added, sensitive paths left unmasked,
/// `/sys`+cgroupfs mounted read-write, every host device passed
/// through, seccomp/AppArmor/SELinux confinement all dropped at once
/// -- a materially bigger increment than this project's own
/// established "one field, one existing OCI spec knob" shape (e.g.
/// `readonly_rootfs`, `0388`) and not attempted here. Matching this
/// project's own established convention instead (every other
/// unsupported request elsewhere in this codebase gets a loud,
/// specific `Status::unimplemented`, never a silent no-op): a clear,
/// honest rejection rather than a confined container masquerading as
/// a privileged one.
fn validate_privileged(
    security_context: Option<&cri::LinuxContainerSecurityContext>,
) -> Result<(), Status> {
    if security_context.is_some_and(|sc| sc.privileged) {
        return Err(Status::unimplemented(
            "privileged containers are not yet supported",
        ));
    }
    Ok(())
}

/// `ContainerConfig.mounts` (0304, closing part of `0237`'s own
/// deferred "CRI mounts" gap) -- a real, deliberately narrow first
/// slice: an ordinary bind mount (`container_path`/`host_path`/
/// `readonly`), matching the exact same `Mount{Type: "bind", Options:
/// ["rbind", ...]}` shape `ociman run -v`'s own `synthesize_spec`
/// already builds. Real cri-o's own much richer `addOCIBindMounts`
/// (`~/git/cri-o/server/container_create_linux.go`) is read directly
/// here for two real, checked-directly behaviors this matches
/// faithfully rather than assuming from the proto's own comments
/// alone: a missing `host_path` is auto-created as a directory
/// (`os.MkdirAll`) rather than treated as an error -- the proto's own
/// doc comment says runtimes "should report an error", but the actual
/// installed cri-o's own real code doesn't, since kubelet's own
/// `HostPath` volumes of type `DirectoryOrCreate` depend on exactly
/// this real runtime behavior; and `PROPAGATION_PRIVATE` (the field's
/// own zero/default value, `types.MountPropagation_PROPAGATION_
/// PRIVATE`) maps to real cri-o's own exact `["rbind", "rprivate"]`
/// option pair.
///
/// Deliberately out of scope for this slice (each a clear,
/// `Status::unimplemented` error rather than a silent
/// misinterpretation, matching this project's own established "narrow
/// first slice" convention): image volume mounts (`Mount.image`, the
/// Image Volume Source KEP -- a real, separate mechanism, not a bind
/// mount at all); any propagation mode other than the private
/// default (`HOST_TO_CONTAINER`/`BIDIRECTIONAL` both need a real
/// shared-mount-namespace setup this project has none of);
/// `selinux_relabel` (this project implements no SELinux concept
/// anywhere, matching `ociman run -v`'s own identical, already-
/// established narrowing); `recursive_read_only`; and any UID/GID
/// mapping (this project has no user-namespace-remapped mount concept
/// for CRI containers at all).
fn build_cri_bind_mounts(
    mounts: &[cri::Mount],
) -> Result<Vec<oci_spec_types::runtime::Mount>, Status> {
    let mut result = Vec::with_capacity(mounts.len());
    for m in mounts {
        // Real cri-o's own exact validation strings
        // (`container_create_linux.go`'s own `addOCIBindMounts`).
        if m.container_path.is_empty() {
            return Err(Status::invalid_argument("mount.ContainerPath is empty"));
        }
        if m.image.is_some() {
            return Err(Status::unimplemented(
                "image volume mounts (Mount.image) are not yet supported",
            ));
        }
        if m.host_path.is_empty() {
            return Err(Status::invalid_argument("mount.HostPath is empty"));
        }
        if m.selinux_relabel {
            return Err(Status::unimplemented(
                "mount.SelinuxRelabel is not yet supported (this project implements no SELinux \
                 concept at all)",
            ));
        }
        if m.propagation != cri::MountPropagation::PropagationPrivate as i32 {
            return Err(Status::unimplemented(
                "mount propagation modes other than the private default are not yet supported",
            ));
        }
        if m.recursive_read_only {
            return Err(Status::unimplemented(
                "mount.RecursiveReadOnly is not yet supported",
            ));
        }
        if !m.uid_mappings.is_empty() || !m.gid_mappings.is_empty() {
            return Err(Status::unimplemented(
                "mount UID/GID mappings are not yet supported",
            ));
        }

        let source = resolve_mount_source(&m.host_path)?;

        let mut options = vec!["rbind".to_string(), "rprivate".to_string()];
        if m.readonly {
            options.push("ro".to_string());
        }
        result.push(oci_spec_types::runtime::Mount {
            destination: m.container_path.clone(),
            source: Some(source),
            kind: Some("bind".to_string()),
            options,
        });
    }
    Ok(result)
}

/// Resolves one `Mount.host_path` to the real path that should
/// actually get bind-mounted -- a real, previously-shipped bug fixed
/// here (0305): [`build_cri_bind_mounts`] used to call
/// `fs::create_dir_all` on *every* `host_path` unconditionally, which
/// fails outright (`EEXIST`) for the common real kubelet case of an
/// already-existing single-file mount source (`/etc/localtime`, a
/// ConfigMap key, `/etc/machine-id`, ...) — confirmed directly with a
/// live repro, not assumed. Matches real cri-o's own exact
/// `resolveSymbolicLink` + conditional-`os.MkdirAll` logic instead
/// (`~/git/cri-o/server/container_create.go`'s own `resolveSymbolicLink`,
/// called from `container_create_linux.go`'s `addOCIBindMounts`):
/// `Lstat` the path first -- an already-existing non-symlink (file or
/// directory) is used exactly as given, never touched; an already-
/// existing symlink is followed to its real target (`fs::canonicalize`
/// here, rather than real cri-o's own `securejoin.SecureJoin`-based
/// confinement, since this project has no `BindMountPrefix`-style
/// redirect concept for that confinement to matter against); only a
/// genuinely *missing* path (`ErrorKind::NotFound`) is auto-created as
/// a directory, matching real cri-o's own `os.IsNotExist` branch
/// exactly (real kubelet `HostPath` volumes of type
/// `DirectoryOrCreate` depend on this). Any other I/O error (e.g.
/// permission denied) is a real, surfaced error rather than silently
/// swallowed.
fn resolve_mount_source(host_path: &str) -> Result<String, Status> {
    match std::fs::symlink_metadata(host_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => std::fs::canonicalize(host_path)
            .map(|p| p.display().to_string())
            .map_err(|e| Status::invalid_argument(format!("resolving symlink {host_path:?}: {e}"))),
        Ok(_) => Ok(host_path.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(host_path).map_err(|e| {
                Status::internal(format!("creating mount source {host_path:?}: {e}"))
            })?;
            Ok(host_path.to_string())
        }
        Err(e) => Err(Status::internal(format!(
            "checking mount source {host_path:?}: {e}"
        ))),
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
        // Real cri-o's own `getHostname` (0292, checked directly
        // against `~/git/cri-o/server/sandbox_run.go`): the sandbox
        // config's own `hostname` if non-empty, else the sandbox id's
        // own first 12 hex chars -- the host-network branch of that
        // same function is unreachable here, since this project has
        // no host-networking concept for a sandbox at all.
        let hostname = if sandbox_config.hostname.is_empty() {
            sb.id[..sb.id.len().min(12)].to_string()
        } else {
            sandbox_config.hostname.clone()
        };
        // Real cri-o's own identical default (`internal/lib/sandbox/
        // infra.go`: `if b.config.GetDnsConfig() == nil { b.config.
        // DnsConfig = &types.DNSConfig{} }`) -- an absent `dns_config`
        // becomes all-empty, which `write_resolv_conf` treats the
        // same way real cri-o's own `ParseDNSOptions` does: copy the
        // real host's own `/etc/resolv.conf` verbatim.
        let empty_dns = Vec::new();
        let (dns_servers, dns_searches, dns_options) = match &sandbox_config.dns_config {
            Some(dns) => (&dns.servers, &dns.searches, &dns.options),
            None => (&empty_dns, &empty_dns, &empty_dns),
        };
        // `ContainerConfig.linux.security_context`'s own `run_as_user`/
        // `run_as_group`/`run_as_username` (0365) -- validated for the
        // exact same "before any real work happens" reason as the
        // mounts check just below.
        validate_run_as_user(
            config
                .linux
                .as_ref()
                .and_then(|l| l.security_context.as_ref()),
        )?;
        // `security_context.privileged` (0389) -- checked right next
        // to `validate_run_as_user`, for the exact same reason.
        validate_privileged(
            config
                .linux
                .as_ref()
                .and_then(|l| l.security_context.as_ref()),
        )?;
        // `security_context.readonly_rootfs` (0388): read here,
        // before `bundle::prepare` builds the spec, the same
        // "resolve every CRI-level input up front" shape every other
        // `CriProcessConfig` field already follows.
        let readonly_rootfs = config
            .linux
            .as_ref()
            .and_then(|l| l.security_context.as_ref())
            .is_some_and(|sc| sc.readonly_rootfs);
        // `ContainerConfig.linux.resources` (0390): translated up
        // front via the same `linux_container_resources_to_oci`
        // `UpdateContainerResources` already uses, so a container
        // actually starts with the requested limits in effect rather
        // than only ever picking them up via a later, separate update
        // call.
        let resources = config
            .linux
            .as_ref()
            .and_then(|l| l.resources.as_ref())
            .map(linux_container_resources_to_oci);
        // `security_context.masked_paths`/`.readonly_paths` (0391),
        // resolved the same way `readonly_rootfs` just above is;
        // empty (the common case, and the only reachable case for a
        // `privileged: true` request, already rejected earlier by
        // `validate_privileged`) when no security context was given.
        let empty_strings: Vec<String> = Vec::new();
        let masked_paths = config
            .linux
            .as_ref()
            .and_then(|l| l.security_context.as_ref())
            .map_or(&empty_strings, |sc| &sc.masked_paths);
        let readonly_paths = config
            .linux
            .as_ref()
            .and_then(|l| l.security_context.as_ref())
            .map_or(&empty_strings, |sc| &sc.readonly_paths);
        // `security_context.capabilities.add_capabilities`/
        // `drop_capabilities` (0392): merged up front, before `bundle::
        // prepare` ever extracts a layer, the same "config-shaped
        // client error should never cost a real, wasted rootfs
        // extraction" reasoning `build_cri_bind_mounts`'s own call site
        // just below already established -- an unknown capability name
        // or a contradictory add/drop request is a real client-input
        // problem (`invalid_argument`), not the generic `internal`
        // `PrepareError::Other` would otherwise map to. `privileged:
        // true` never reaches here at all (already rejected earlier by
        // `validate_privileged`), so the base set is always this
        // project's own real `podman`-default set, matching `ociman
        // run`'s own identical non-privileged branch.
        let requested_caps = config
            .linux
            .as_ref()
            .and_then(|l| l.security_context.as_ref())
            .and_then(|sc| sc.capabilities.as_ref());
        let capabilities = oci_runtime_core::identity::merge_capabilities(
            &oci_spec_types::runtime::podman_default_capabilities(),
            requested_caps.map_or(&empty_strings, |c| &c.add_capabilities),
            requested_caps.map_or(&empty_strings, |c| &c.drop_capabilities),
        )
        .map_err(|e| Status::invalid_argument(e.to_string()))?;
        // `PodSandboxConfig.linux.sysctls` (0396) -- a real, sandbox-
        // level CRI concept, not read from `config` (the per-
        // container request) at all; see `CriProcessConfig::sysctl`'s
        // own doc comment for exactly why this project applies it
        // per-container instead of once for the whole sandbox. Real
        // per-key validation (whether it's even namespaced, and
        // whether this container's own declared namespaces satisfy
        // it) happens later, for free, inside `oci_runtime_core::
        // launch` itself (`0395`) -- nothing to check here at all.
        let sysctl: std::collections::BTreeMap<String, String> = sandbox_config
            .linux
            .as_ref()
            .map(|l| l.sysctls.clone().into_iter().collect())
            .unwrap_or_default();
        // `ContainerConfig.mounts` (0304): validated and translated to
        // plain bind mounts *before* `bundle::prepare` ever extracts a
        // single layer -- a config-shaped client error here should
        // never cost a real, wasted rootfs extraction, the same
        // reasoning `build_spec`'s own doc comment already establishes
        // for `PrepareError::NoCommand`.
        let mounts = build_cri_bind_mounts(&config.mounts)?;
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
                hostname: &hostname,
                dns_servers,
                dns_searches,
                dns_options,
                mounts: &mounts,
                readonly_rootfs,
                resources,
                masked_paths,
                readonly_paths,
                capabilities,
                sysctl,
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

        // The CRI log path (0242): kubelet's own convention is the
        // sandbox config's `log_directory` joined with the container
        // config's `log_path` -- only when both are given (crictl
        // routinely gives neither), matching real cri-o's own
        // `filepath.Join(sb.LogDir(), containerConfig.GetLogPath())`.
        let log_path = (!sandbox_config.log_directory.is_empty() && !config.log_path.is_empty())
            .then(|| {
                std::path::Path::new(&sandbox_config.log_directory)
                    .join(&config.log_path)
                    .display()
                    .to_string()
            });

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
            log_path,
            // The image's own STOPSIGNAL (0244) -- image_config was
            // already read above for the bundle spec.
            stop_signal: image_config.stop_signal.clone().filter(|s| !s.is_empty()),
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
        let mut command = std::process::Command::new(exe);
        command
            .arg(crate::launcher::LAUNCH_ARGV1)
            .arg(&bundle_dir)
            .arg(&record.id);
        // The CRI log path (0242), when kubelet configured one -- the
        // launcher wires the container's stdout/stderr into its own
        // logger process writing the real CRI-format file there.
        if let Some(log_path) = &record.log_path {
            command.arg(log_path);
        }
        let mut child = command
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
            // The image's own STOPSIGNAL when declared (0244), else
            // SIGTERM -- with real cri-o's own garbage-tolerant TERM
            // fallback for an unparsable declaration
            // (`Container::StopSignal`, checked directly).
            let graceful_signal = record
                .stop_signal
                .as_deref()
                .and_then(|s| oci_runtime_core::signal::parse(s).ok())
                .unwrap_or(libc::SIGTERM);
            let _ = oci_runtime_core::process::kill(pid, graceful_signal);
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

    /// The `CRIListStreaming` variant of `list_containers` — the exact
    /// same filtered-list computation, streamed in chunks of real
    /// cri-o's own `streamChunkSize` (see `docs/design/0234`/`0253`
    /// and `stream.rs`'s own module doc comment — an empty result
    /// streams zero messages and closes immediately, matching real
    /// cri-o's own `StreamContainers` exactly), completing the same
    /// `CRIListStreaming` family `StreamPodSandboxes`/`StreamImages`
    /// already did.
    async fn stream_containers(
        &self,
        request: Request<cri::StreamContainersRequest>,
    ) -> Result<Response<Self::StreamContainersStream>, Status> {
        let containers = container_list_items(request.into_inner().filter)?;
        Ok(Response::new(crate::stream::chunked(
            containers,
            |containers| cri::StreamContainersResponse { containers },
        )))
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
                log_path: record.log_path.clone().unwrap_or_default(),
                ..Default::default()
            }),
            info,
        }))
    }

    /// Real cri-o (`server/container_update_resources.go`, checked
    /// directly) applies this to both `Running` and `Created`
    /// containers, since its own runtime layer already gives every
    /// created container a live cgroup to write into. This project's
    /// own `CreateContainer` deliberately doesn't (`docs/design/
    /// 0237`'s own note: cgroup/process setup is `StartContainer`'s
    /// job) — a `Created` container here has no live cgroup at all,
    /// so honestly there is nothing yet to update; `Running` is the
    /// only state this can act on for real, matching the "absence
    /// over fabrication" rule this project already applies elsewhere
    /// (e.g. `ContainerStats`, `docs/design/0241`) rather than
    /// silently accepting a `Created`-state request that changes
    /// nothing.
    ///
    /// Field mapping onto [`oci_spec_types::runtime::LinuxResources`]
    /// (the exact same shape `ociman update`/`ocirun update` already
    /// apply via `oci_runtime_core::cgroups::plan_resources`/`apply`):
    /// `cpu_shares`/`cpu_period`/`cpu_quota`/`cpuset_cpus`/
    /// `cpuset_mems` map straight across (`0`/`""` meaning "not
    /// specified", the proto's own documented default); `memory_
    /// limit_in_bytes`/`memory_swap_limit_in_bytes` map onto `memory.
    /// limit`/`memory.swap` directly, honoring the request's own
    /// explicit swap value (unlike real cri-o's own checked
    /// `toOCIResources`, which curiously pins `Memory.Swap` to the
    /// *limit* value whenever swap accounting is available at all and
    /// never actually reads `GetMemorySwapLimitInBytes()` -- honoring
    /// what the caller actually asked for is more correct than
    /// replicating that). `oom_score_adj`/`hugepage_limits`/`unified`
    /// have no home yet (no oom-score-adj write path, no hugetlb
    /// support, no raw-cgroup-v2-file passthrough anywhere in this
    /// project) and are honestly ignored rather than silently
    /// mis-applied -- matching `oci_runtime_core::cgroups`' own
    /// existing, narrower-than-the-full-spec scope.
    async fn update_container_resources(
        &self,
        request: Request<cri::UpdateContainerResourcesRequest>,
    ) -> Result<Response<cri::UpdateContainerResourcesResponse>, Status> {
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
        if record.state != container::ContainerState::Running {
            return Err(Status::failed_precondition(format!(
                "container {} is not running (resources can only be updated on a live cgroup)",
                record.id
            )));
        }
        let Some(linux) = request.linux else {
            // Nothing to do -- matches real cri-o's own identical
            // no-op when the (optional) Linux half of the request is
            // absent.
            return Ok(Response::new(cri::UpdateContainerResourcesResponse {}));
        };

        let pid = record
            .pid
            .ok_or_else(|| Status::internal(format!("container {id} has no recorded pid")))?;
        let cgroup_dir = oci_runtime_core::cgroups::cgroup_dir_for_running_pid(
            std::path::Path::new("/sys/fs/cgroup"),
            pid,
        )
        .map_err(|e| Status::internal(format!("resolving cgroup for container {id}: {e}")))?;

        let resources = linux_container_resources_to_oci(&linux);
        let writes = oci_runtime_core::cgroups::plan_resources(&resources);
        oci_runtime_core::cgroups::apply(&cgroup_dir, &writes)
            .map_err(|e| Status::internal(format!("updating resources for container {id}: {e}")))?;

        Ok(Response::new(cri::UpdateContainerResourcesResponse {}))
    }

    /// Log rotation's other half (`docs/design/0243`): kubelet renames
    /// the CRI log file away, then calls this so subsequent lines land
    /// in a fresh file at the same path. Implemented exactly the way
    /// real cri-o drives real conmon (checked directly,
    /// `internal/oci/runtime_oci.go`: a command written to conmon's
    /// own control fifo in the bundle path): one byte into the
    /// logger's own `logger-ctl` fifo. Semantics match cri-o's own
    /// RPC: unknown container an error, "container is not running"
    /// for anything not running — plus one honest narrowing: a
    /// running container that was never given a log path has no
    /// logger (and no log to rotate), a clear error rather than a
    /// silent success.
    async fn reopen_container_log(
        &self,
        request: Request<cri::ReopenContainerLogRequest>,
    ) -> Result<Response<cri::ReopenContainerLogResponse>, Status> {
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
            reconcile_container(record)?
        };
        if record.state != container::ContainerState::Running {
            // Real cri-o's own message, verbatim.
            return Err(Status::failed_precondition("container is not running"));
        }
        if record.log_path.is_none() {
            return Err(Status::failed_precondition(format!(
                "container {} has no log path (no logger to reopen)",
                record.id
            )));
        }

        let ctl_path =
            crate::bundle::bundle_dir(&oci_cli_common::storage::default_root(), &record.id)
                .join(crate::launcher::LOGGER_CTL_FILENAME);
        // Nonblocking write-only open: succeeds only while the logger
        // is actually reading (a real liveness check -- `ENXIO`
        // otherwise); retried briefly to cover the logger's own
        // between-commands re-open window. Runs on the blocking pool
        // (real filesystem waits).
        tokio::task::spawn_blocking(move || -> Result<(), Status> {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            loop {
                match rustix::fs::open(
                    &ctl_path,
                    rustix::fs::OFlags::WRONLY | rustix::fs::OFlags::NONBLOCK,
                    rustix::fs::Mode::empty(),
                ) {
                    Ok(fd) => {
                        rustix::io::write(&fd, b"r").map_err(|e| {
                            Status::internal(format!("writing reopen command: {e}"))
                        })?;
                        return Ok(());
                    }
                    // ENXIO: no reader right now -- either the logger
                    // is between control rounds (retry) or genuinely
                    // gone (give up at the deadline).
                    Err(rustix::io::Errno::NXIO) => {
                        if std::time::Instant::now() >= deadline {
                            return Err(Status::internal(
                                "the container's logger is not listening (ENXIO)",
                            ));
                        }
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                    Err(e) => {
                        return Err(Status::internal(format!(
                            "opening logger control fifo: {e}"
                        )));
                    }
                }
            }
        })
        .await
        .map_err(|e| Status::internal(format!("reopen task panicked: {e}")))??;

        Ok(Response::new(cri::ReopenContainerLogResponse {}))
    }

    /// Runs a command synchronously inside a running container
    /// (`docs/design/0240`) — kubelet's own exec liveness/readiness
    /// probes. Semantics checked directly against real cri-o
    /// (`server/container_execsync.go`, `internal/oci/runtime_oci.
    /// go`): unknown container a real `NotFound`; a container with no
    /// living process a `NotFound` too (real cri-o can exec into its
    /// own *created* containers — their paused init is alive; this
    /// project's created containers have no process at all yet, so
    /// only `RUNNING` is exec-able here, documented in the note);
    /// an empty command its verbatim error; and a timeout is a
    /// **successful response** with `exit_code: -1` and stderr
    /// `"command timed out"` (conmon's own `TimedOutMessage`,
    /// verbatim) — real cri-o's own explicit reasoning: kubelet's
    /// prober checks the exit code, and a gRPC error would wedge the
    /// probe in `Unknown` instead of restarting the container.
    async fn exec_sync(
        &self,
        request: Request<cri::ExecSyncRequest>,
    ) -> Result<Response<cri::ExecSyncResponse>, Status> {
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
        if record.state != container::ContainerState::Running {
            return Err(Status::not_found(format!(
                "container {} has no running process to exec into (state: {:?})",
                record.id, record.state
            )));
        }
        let Some(pid) = record.pid else {
            return Err(Status::internal(format!(
                "container {} is RUNNING but has no recorded pid",
                record.id
            )));
        };
        if request.cmd.is_empty() {
            // Real cri-o's own message, verbatim.
            return Err(Status::invalid_argument("exec command cannot be empty"));
        }

        let bundle_dir =
            crate::bundle::bundle_dir(&oci_cli_common::storage::default_root(), &record.id);
        let exe = std::env::current_exe()
            .map_err(|e| Status::internal(format!("resolving own executable: {e}")))?;
        let timeout = request.timeout;
        let cmd = request.cmd;

        // The whole child-wrangling half runs on the blocking pool:
        // real pipe reads, a try_wait poll loop, and (on timeout) a
        // whole-process-group SIGKILL -- see `launcher::exec_main`'s
        // own doc comment for why the helper `setsid`s to make that
        // group kill cover the namespace-joined exec child too.
        let outcome = tokio::task::spawn_blocking(move || -> std::io::Result<ExecOutcome> {
            let mut child = std::process::Command::new(exe)
                .arg(crate::launcher::EXEC_ARGV1)
                .arg(&bundle_dir)
                .arg(pid.to_string())
                .args(&cmd)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()?;

            // Drain both pipes on their own threads -- reading only
            // after exit could deadlock once a pipe fills.
            let mut stdout_pipe = child.stdout.take().expect("stdout was piped");
            let mut stderr_pipe = child.stderr.take().expect("stderr was piped");
            let stdout_reader = std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = std::io::Read::read_to_end(&mut stdout_pipe, &mut buf);
                buf
            });
            let stderr_reader = std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = std::io::Read::read_to_end(&mut stderr_pipe, &mut buf);
                buf
            });

            let deadline = (timeout > 0).then(|| {
                std::time::Instant::now() + std::time::Duration::from_secs(timeout as u64)
            });
            let mut timed_out = false;
            let status = loop {
                if let Some(status) = child.try_wait()? {
                    break status;
                }
                if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
                    timed_out = true;
                    // The helper setsid'd: its pid is its process
                    // group, so this takes the exec'd command down
                    // with it.
                    let _ = oci_runtime_core::process::kill(-(child.id() as i32), libc::SIGKILL);
                    break child.wait()?;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            };

            let stdout = stdout_reader.join().unwrap_or_default();
            let stderr = stderr_reader.join().unwrap_or_default();
            let exit_code = std::os::unix::process::ExitStatusExt::signal(&status)
                .map(|sig| 128 + sig)
                .or(status.code())
                .unwrap_or(-1);
            Ok(ExecOutcome {
                timed_out,
                exit_code,
                stdout,
                stderr,
            })
        })
        .await
        .map_err(|e| Status::internal(format!("exec task panicked: {e}")))?
        .map_err(|e| Status::internal(format!("running exec helper: {e}")))?;

        if outcome.timed_out {
            // Real cri-o's own timeout shape, verbatim (see this
            // method's doc comment).
            return Ok(Response::new(cri::ExecSyncResponse {
                stdout: Vec::new(),
                stderr: b"command timed out".to_vec(),
                exit_code: -1,
            }));
        }
        Ok(Response::new(cri::ExecSyncResponse {
            stdout: outcome.stdout,
            stderr: outcome.stderr,
            exit_code: outcome.exit_code,
        }))
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

    /// Real, live cgroup-backed stats for one container
    /// (`docs/design/0241`, see [`container_stats_for`]) — an unknown
    /// ID is an error (real cri-o's own `ContainerStats`), a known
    /// container without live cgroup accounting (created/exited, or a
    /// no-cgroup rootless fallback launch) is a real response with no
    /// stats, never a fabricated zero row.
    async fn container_stats(
        &self,
        request: Request<cri::ContainerStatsRequest>,
    ) -> Result<Response<cri::ContainerStatsResponse>, Status> {
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
            reconcile_container(record)?
        };
        Ok(Response::new(cri::ContainerStatsResponse {
            stats: container_stats_for(&record),
        }))
    }

    /// Stats for every container matching the filter — see
    /// [`container_stats_items`] for the exact filter/absence rules.
    async fn list_container_stats(
        &self,
        request: Request<cri::ListContainerStatsRequest>,
    ) -> Result<Response<cri::ListContainerStatsResponse>, Status> {
        let stats = container_stats_items(request.into_inner().filter)?;
        Ok(Response::new(cri::ListContainerStatsResponse { stats }))
    }

    type StreamContainerStatsStream = BoxStream<cri::StreamContainerStatsResponse>;

    /// The `CRIListStreaming` variant of `list_container_stats` — the
    /// same shared chunking every other streaming list RPC here uses
    /// (0234).
    async fn stream_container_stats(
        &self,
        request: Request<cri::StreamContainerStatsRequest>,
    ) -> Result<Response<Self::StreamContainerStatsStream>, Status> {
        let items = container_stats_items(request.into_inner().filter)?;
        Ok(Response::new(crate::stream::chunked(items, |stats| {
            cri::StreamContainerStatsResponse {
                container_stats: stats,
            }
        })))
    }

    /// An unknown sandbox is a real `NotFound` (matching real cri-o's
    /// own `getPodSandboxFromRequest` wrapping, the same rule
    /// `UpdatePodSandboxResources`/`ReopenContainerLog` already use);
    /// a real, existing one always gets a real response — see
    /// [`pod_sandbox_stats_items`]'s own doc comment for exactly what
    /// it reports and why `linux` is always absent.
    async fn pod_sandbox_stats(
        &self,
        request: Request<cri::PodSandboxStatsRequest>,
    ) -> Result<Response<cri::PodSandboxStatsResponse>, Status> {
        let id = request.into_inner().pod_sandbox_id;
        if find_sandbox(&id)?.is_none() {
            return Err(Status::not_found(format!("could not find pod {id:?}")));
        }
        let mut stats = pod_sandbox_stats_items(Some(cri::PodSandboxStatsFilter {
            id,
            label_selector: Default::default(),
        }))?;
        Ok(Response::new(cri::PodSandboxStatsResponse {
            stats: stats.pop(),
        }))
    }

    /// Stats for every sandbox matching the filter — see
    /// [`pod_sandbox_stats_items`] for the exact filter/absence rules.
    async fn list_pod_sandbox_stats(
        &self,
        request: Request<cri::ListPodSandboxStatsRequest>,
    ) -> Result<Response<cri::ListPodSandboxStatsResponse>, Status> {
        let stats = pod_sandbox_stats_items(request.into_inner().filter)?;
        Ok(Response::new(cri::ListPodSandboxStatsResponse { stats }))
    }

    type StreamPodSandboxStatsStream = BoxStream<cri::StreamPodSandboxStatsResponse>;

    /// The `CRIListStreaming` variant of `list_pod_sandbox_stats` —
    /// the same shared chunking every other streaming list RPC here
    /// uses (0234).
    async fn stream_pod_sandbox_stats(
        &self,
        request: Request<cri::StreamPodSandboxStatsRequest>,
    ) -> Result<Response<Self::StreamPodSandboxStatsStream>, Status> {
        let items = pod_sandbox_stats_items(request.into_inner().filter)?;
        Ok(Response::new(crate::stream::chunked(items, |stats| {
            cri::StreamPodSandboxStatsResponse {
                pod_sandbox_stats: stats,
            }
        })))
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

    /// Checked directly against real cri-o's own `server/
    /// container_checkpoint.go` before writing anything: real cri-o's
    /// own config actually *defaults* `EnableCriuSupport` to `true`
    /// (`pkg/config/config.go`'s own `DefaultConfig`) — but at
    /// startup it's force-disabled again unless a real `criu` binary
    /// is actually found on `$PATH` (`validateCriuInPath`), which
    /// essentially no host has installed by default (checkpoint/
    /// restore is a niche, opt-in capability, not a standard
    /// dependency). So the overwhelmingly common real behavior is
    /// still disabled either way — real cri-o's own bare `errors.New
    /// ("checkpoint/restore support not available")` (never wrapped
    /// in a `status.Error`, so real gRPC surfaces it as `codes.
    /// Unknown`, not some more specific code) before ever resolving
    /// the container or touching anything else.
    ///
    /// This project has no CRIU/checkpoint-restore integration at all
    /// (a real container checkpoint needs matching podman/cri-o's own
    /// checkpoint archive format field for field — a materially large
    /// feature, deliberately out of scope) — a structurally different
    /// reason than real cri-o's own "usually-missing binary" one, but
    /// the exact same honest, observable answer either way: a real
    /// error, not a silent success or a fabricated checkpoint. Uses
    /// real cri-o's own identical message/status code rather than a
    /// generic `Status::unimplemented`, since that *is* what a real,
    /// unconfigured `cri-o` install would actually return here too.
    async fn checkpoint_container(
        &self,
        _request: Request<cri::CheckpointContainerRequest>,
    ) -> Result<Response<cri::CheckpointContainerResponse>, Status> {
        Err(Status::unknown("checkpoint/restore support not available"))
    }

    type GetContainerEventsStream = BoxStream<cri::ContainerEventResponse>;

    /// Real cri-o's own `GetContainerEvents` (`server/
    /// container_events.go`, checked directly) is entirely gated
    /// behind its own `enable_pod_events` config toggle — a plain
    /// `bool` with no explicit default assignment anywhere in
    /// `pkg/config/config.go`'s own `DefaultConfig` (Go's own zero
    /// value, `false`), so a real, unconfigured `cri-o` install has it
    /// off by default. When disabled, its own implementation returns
    /// `nil` immediately: a real, clean stream that closes with zero
    /// messages, never actually blocking to wait for an event at all.
    ///
    /// This project has no event-generation machinery, or an
    /// `enable_pod_events`-equivalent toggle, anywhere — so the
    /// honest, real-cri-o-default-matching answer is that identical
    /// immediately-closed, empty stream, not a hard `Status::
    /// unimplemented` (which real cri-o's own default install would
    /// never actually return for this RPC) — the same "absence over
    /// fabrication" reasoning `ListPodSandboxMetrics`/
    /// `StreamPodSandboxMetrics` already established (`docs/design/
    /// 0255`). A real per-container event bus (needed the moment this
    /// project ever wants `enable_pod_events`-style behavior turned
    /// *on*) is a materially bigger feature, deliberately still ahead.
    async fn get_container_events(
        &self,
        _request: Request<cri::GetEventsRequest>,
    ) -> Result<Response<Self::GetContainerEventsStream>, Status> {
        Ok(Response::new(Box::pin(tokio_stream::empty())))
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
    /// anywhere (`ListPodSandboxMetrics`/`StreamPodSandboxMetrics`,
    /// below, report a real, honest empty list for the identical
    /// reason — see their own doc comments) — so advertising even
    /// that one always-on descriptor here would be a real, false
    /// claim: a caller could reasonably expect a following
    /// `ListPodSandboxMetrics` call to actually return a value for
    /// whatever this RPC just told it exists. An empty list is
    /// genuinely the most honest possible answer, not a placeholder —
    /// real cri-o's own architecture already establishes that
    /// returning nothing here is a normal, valid, unconfigured-install
    /// response, not an error condition kubelet needs to special-case.
    async fn list_metric_descriptors(
        &self,
        _request: Request<cri::ListMetricDescriptorsRequest>,
    ) -> Result<Response<cri::ListMetricDescriptorsResponse>, Status> {
        Ok(Response::new(cri::ListMetricDescriptorsResponse {
            descriptors: Vec::new(),
        }))
    }

    /// A real, honest empty list, exactly matching `ListMetricDescriptors`'s
    /// own reasoning (`docs/design/0255`) — checked directly against
    /// real cri-o's own `server/sandbox_metrics_list.go`: its own
    /// `listPodSandboxMetrics` walks every real sandbox and asks the
    /// stats subsystem for that sandbox's own computed metric, but
    /// with no `included_pod_metrics` configured (this project's own
    /// only real point of comparison — real cri-o's own default too)
    /// that computed metric is always absent, so every sandbox
    /// contributes nothing and the real, unconfigured answer is a
    /// plain empty list — never an error, and never one entry per
    /// sandbox with empty fields either. `ocicri` has the identical
    /// real gap (no metrics-collection machinery at all), so the
    /// honest, real-cri-o-matching answer here is this same empty
    /// list unconditionally, not a hard failure — a genuine
    /// correctness improvement over an earlier `Status::unimplemented`
    /// placeholder, which real cri-o's own unconfigured install would
    /// never actually return for this RPC.
    async fn list_pod_sandbox_metrics(
        &self,
        _request: Request<cri::ListPodSandboxMetricsRequest>,
    ) -> Result<Response<cri::ListPodSandboxMetricsResponse>, Status> {
        Ok(Response::new(cri::ListPodSandboxMetricsResponse {
            pod_metrics: Vec::new(),
        }))
    }

    type StreamPodSandboxMetricsStream = BoxStream<cri::StreamPodSandboxMetricsResponse>;

    /// The `CRIListStreaming` sibling of [`list_pod_sandbox_metrics`] —
    /// see its own doc comment for exactly why an unconditional empty
    /// list is the real, honest answer here too; zero messages before
    /// a clean EOF, matching every other `CRIListStreaming` RPC's own
    /// identical empty-input behavior (`stream.rs`'s own module doc
    /// comment).
    ///
    /// [`list_pod_sandbox_metrics`]: RuntimeServiceImpl::list_pod_sandbox_metrics
    async fn stream_pod_sandbox_metrics(
        &self,
        _request: Request<cri::StreamPodSandboxMetricsRequest>,
    ) -> Result<Response<Self::StreamPodSandboxMetricsStream>, Status> {
        Ok(Response::new(crate::stream::chunked(
            Vec::new(),
            |pod_sandbox_metrics| cri::StreamPodSandboxMetricsResponse {
                pod_sandbox_metrics,
            },
        )))
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

    /// `UpdateContainerResources`'s own pod-level sibling (`docs/
    /// design/0254`) — checked directly against real cri-o's own
    /// `server/sandbox_update_resources.go`: beyond resolving the
    /// sandbox (a real `NotFound` when it doesn't exist, matching its
    /// own `getPodSandboxFromRequest` wrapping exactly), real cri-o's
    /// own implementation does *nothing* to the sandbox's own cgroup
    /// directly here at all — every actual resource change is
    /// delegated entirely to its own optional NRI (Node Resource
    /// Interface) plugin framework (`s.nri.updatePodSandbox`), which
    /// is a real, honest no-op with no plugins configured (the
    /// default, and the only configuration either project's own CI
    /// ever runs). This project has no NRI concept at all — a
    /// materially bigger, separate plugin framework, entirely out of
    /// scope — so once the sandbox is confirmed to exist, this is
    /// honestly exactly that same no-op, not a fabricated cgroup
    /// write this project has nowhere to route (unlike
    /// `UpdateContainerResources`, `ocicri` has no per-sandbox cgroup
    /// of its own to write into at all: see `docs/design/0233`'s own
    /// "no infra process" note).
    async fn update_pod_sandbox_resources(
        &self,
        request: Request<cri::UpdatePodSandboxResourcesRequest>,
    ) -> Result<Response<cri::UpdatePodSandboxResourcesResponse>, Status> {
        let id = request.into_inner().pod_sandbox_id;
        if find_sandbox(&id)?.is_none() {
            return Err(Status::not_found(format!("could not find pod {id:?}")));
        }
        Ok(Response::new(cri::UpdatePodSandboxResourcesResponse {}))
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

    /// `validate_privileged` (0389): no security context at all, or
    /// one with `privileged: false` (the common, unconfigured
    /// default), must succeed; an explicit `privileged: true` request
    /// must be a clear, honest `Status::unimplemented` rather than a
    /// silent no-op.
    #[test]
    fn validate_privileged_rejects_an_explicit_true_but_allows_everything_else() {
        assert!(validate_privileged(None).is_ok());
        assert!(
            validate_privileged(Some(&cri::LinuxContainerSecurityContext {
                privileged: false,
                ..Default::default()
            }))
            .is_ok()
        );
        let status = validate_privileged(Some(&cri::LinuxContainerSecurityContext {
            privileged: true,
            ..Default::default()
        }))
        .unwrap_err();
        assert_eq!(status.code(), tonic::Code::Unimplemented);
        assert!(
            status
                .message()
                .contains("privileged containers are not yet supported"),
            "{status:?}"
        );
    }
}
