# Design note 0251: `ocicri UpdateContainerResources`

Status: implemented
Scope: `bin/ocicri/src/runtime_service.rs`, `tests/tests/ocicri_container.rs`.

## What this RPC is for

`UpdateContainerResources` is a real CRI v1 `RuntimeService` RPC kubelet
calls for in-place container resource changes (vertical pod resizing on
recent Kubernetes, or simply a config update without a restart). Until
this slice it was a real, honest `Status::unimplemented` like every
other not-yet-done `RuntimeService` RPC — a hard failure for any CRI
client that calls it, exactly the kind of drop-in-replacement gap
milestone 7 exists to close.

## Composition, not new engineering

Every piece this needed already existed and was already proven
elsewhere in the workspace:

- `find_container`/`reconcile_container` — the same container
  resolution and state-settling every other mutating container RPC
  (`StopContainer`, `ReopenContainerLog`, ...) already uses.
- `oci_runtime_core::cgroups::cgroup_dir_for_running_pid` — already
  used by `container_stats_for` (`docs/design/0241`) to resolve a
  running container's live cgroup directory from its recorded pid.
- `oci_runtime_core::cgroups::plan_resources`/`apply` — the *exact*
  same two calls `ociman update`'s `cmd_update` already makes; this
  slice adds no new cgroup-writing logic of its own at all, just a
  new caller.

The only genuinely new code is the pure conversion function
(`linux_container_resources_to_oci`) mapping CRI's
`LinuxContainerResources` onto `oci_spec_types::runtime::LinuxResources`.

## Scope decisions, checked against real cri-o directly

- **State**: real cri-o (`server/container_update_resources.go`,
  checked directly) accepts both `Running` and `Created`, because its
  own runtime layer already gives a `Created` container a live cgroup
  to write into. This project's own `CreateContainer` deliberately
  doesn't set one up yet (`docs/design/0237`: cgroup/process creation
  is `StartContainer`'s job) — a `Created` container here has no live
  cgroup at all, so there is honestly nothing to update. Rather than
  silently accepting a `Created`-state request that changes nothing,
  this returns a real `FailedPrecondition`, matching this project's
  own established "absence over fabrication" rule (the same reasoning
  `ContainerStats`, 0241, already applies).
- **Memory swap**: real cri-o's own `toOCIResources` curiously never
  reads the request's own `GetMemorySwapLimitInBytes()` at all — it
  pins `Memory.Swap` to the *limit* value whenever swap accounting is
  available, ignoring whatever the caller actually asked for. This
  slice honors the request's own explicit `memory_swap_limit_in_bytes`
  value directly instead: matching an apparent real-cri-o oversight
  for its own sake would be less correct, not more compatible, since a
  CRI client that actually sets this field would get a silently wrong
  result from real cri-o and a correct one here.
- **`oom_score_adj`/`hugepage_limits`/`unified`**: none has a home
  anywhere in `oci_runtime_core::cgroups` yet (no oom-score-adj write
  path, no hugetlb support, no raw-cgroup-v2-file passthrough) — the
  same, already-established narrower scope every other resource path
  in this project already has. Honestly ignored rather than silently
  mis-applied.
- **Absent `linux`**: a real, documented no-op, matching real cri-o's
  own identical behavior (its own `if req.GetLinux() != nil` guard).

## Verified

Integration (`tests/tests/ocicri_container.rs`,
`update_container_resources_changes_the_real_live_cgroup`, gated on a
reachable `systemd --user` session like the neighboring stats test):

- An unknown container ID is a real gRPC `NotFound`.
- A created-but-never-started container is a real `FailedPrecondition`
  (no live cgroup to act on).
- A running container's real `memory.max`/`cpu.max` cgroup files
  genuinely change to the requested values, read back directly from
  `/sys/fs/cgroup` (not just trusting the RPC's own empty success
  response) — the same direct-cgroup-file assertion style
  `ociman_update.rs`'s own
  `update_changes_the_real_live_cgroup_limits_of_a_running_container`
  already established. `cpuset.cpus`/`cpuset.mems` are deliberately
  not exercised here, for the same already-documented reason neither
  `ociman_update.rs` nor `ocirun_update.rs` does either (the `cpuset`
  controller isn't always delegated into a real user systemd session's
  own cgroup subtree).
- An absent `Linux` half is a harmless no-op, never an error.

Full workspace: `cargo build`/`test --workspace`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`python3 ci/guards.py`, `cargo deny check`, `ci/native-ci.sh`,
`ci/build-deb.sh`.

## Still ahead

`RuntimeService`'s remaining real gaps: `Exec`/`Attach`/`PortForward`
(the streaming-server URL RPCs, a materially bigger feature),
`StreamContainers`, `PodSandboxStats`/`ListPodSandboxStats`/
`StreamPodSandboxStats`, `CheckpointContainer`, `GetContainerEvents`,
`ListPodSandboxMetrics`/`StreamPodSandboxMetrics`, and
`UpdatePodSandboxResources` (this RPC's own pod-level sibling).
