# Design note 0241: `ocicri` container stats

Status: implemented
Scope: `crates/oci-runtime-core/src/cgroups.rs`,
`bin/ocicri/src/runtime_service.rs`, `tests/tests/ocicri_container.rs`.

## The metrics RPCs kubelet polls continuously

With containers really running (0238), `ContainerStats`/
`ListContainerStats` are what kubelet's resource pipeline polls every
few seconds for every container. Like `ExecSync` (0240), they map
almost entirely onto machinery this project already shares: the
`oci_runtime_core::cgroups` readers `ociman stats` already uses
(`cgroup_dir_for_running_pid`, `cpu_usage_nanos`,
`memory_usage_bytes`, `pids_current`, ...), plus `oci_store::
dir_stats` (the same hardlink-aware walk `ImageFsInfo` uses) for the
writable layer.

## Field mapping, checked directly against real cri-o

(`internal/lib/statsserver/stats_server_linux.go`, its cgroup-v2
branch):

- `cpu.usage_core_nano_seconds` — total usage; cgroup v2's
  `cpu.stat` `usage_usec × 1000` (the shared `cpu_usage_nanos`).
- `memory.working_set_bytes` — usage − `inactive_file`, clamped at
  zero: exactly what the shared `memory_usage_bytes` already computes
  (itself checked against podman's own `memoryStat` when it was
  written; cri-o's `computeMemoryStats` is the same docker/cAdvisor
  convention).
- `memory.usage_bytes` — the *raw* `memory.current` (new shared
  helper `memory_current_bytes`); `rss = anon`,
  `page_faults = pgfault`, `major_page_faults = pgmajfault` (new
  shared helper `memory_stat_key`); `available_bytes` only when a
  real `memory.max` limit exists (cri-o's own `isMemoryUnlimited`
  guard).
- `writable_layer` — the bundle rootfs's real disk usage
  (mountpoint + used bytes + inodes via `dir_stats`).
- `usage_nano_cores` — **deliberately never fabricated**: real cri-o
  derives it from two samples over time (`updateUsageNanoCores`'s own
  cache); a single-shot reader has no honest value to put there, and
  kubelet computes rates from `usage_core_nano_seconds` itself.
- `psi`/`swap`/`io` — not read yet (PSI is optional kernel accounting
  real cri-o also only reports when available), left `None`.

## Absence over fabrication

Stats exist only for a container with live, readable cgroup
accounting: created/exited containers, a pid that died mid-read, and
a launch whose systemd-scope setup fell back to no cgroup at all (the
documented rootless no-D-Bus fallback) all yield *no stats* — absent
from the list, and a `stats: None` response (never an error, never a
zero row) for the single-container RPC, whose unknown-ID case is a
real `NotFound` (real cri-o's own `ContainerStats` errors there too).

`StreamContainerStats` (the `CRIListStreaming` sibling) lands with
its list form, sharing the same filtered computation through 0234's
chunking — `ContainerStatsFilter` has the same `id`/`pod_sandbox_id`/
`label_selector` fields as the container-list filter (no state
field), with the same AND/prefix rules.

`PodSandboxStats`/`ListPodSandboxStats` stay honestly unimplemented:
a sandbox here has no process/cgroup of its own at all yet (0233),
and its network namespace (the other thing real cri-o reports there)
doesn't exist either.

## Verified

- New shared-crate unit tests: `memory_current_bytes` reads raw
  (no inactive-file subtraction), `memory_stat_key` reads named keys
  with the missing-key-is-zero tolerance.
- Integration (real socket, real running container, skipped cleanly
  without a `systemd --user` session — the same probe
  `ociman_stats.rs` uses): a running sleeper reports a real
  `usage_core_nano_seconds` reading, nonzero `working_set_bytes`,
  `usage_bytes >= working_set_bytes` (the subtraction is real), a
  writable layer whose mountpoint is the real bundle rootfs with real
  bytes; a created-only container gets `stats: None` and is absent
  from the list; the sandbox prefix filter works; the stream matches
  the list; unknown ID is `NotFound`.
- Full workspace: `cargo build`, `cargo test --workspace`,
  `cargo fmt --all --check`, `cargo clippy --workspace --all-targets
  -- -D warnings`, `python3 ci/guards.py`, `cargo deny check`,
  `bash ci/native-ci.sh`, `ci/build-deb.sh`, `ci/bench.sh` sanity
  (the two new cgroup helpers are cold-path additions; nothing on any
  startup path changed).
