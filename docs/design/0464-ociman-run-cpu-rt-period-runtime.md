# Design note 0464: `ociman run`/`create`/`update --cpu-rt-period`/`--cpu-rt-runtime`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `bin/ociman/src/build.rs`, `tests/
tests/ociman_run.rs`, `tests/tests/ociman_update.rs`.

## What this closes

`ociman run`/`create`/`update` had no `--cpu-rt-period`/`--cpu-rt-
runtime` flags of their own at all — real `docker run`/`podman run`/
`create`/`update`'s own realtime-scheduling CPU controls. `ocirun
update --cpu-rt-period`/`--cpu-rt-runtime` (`0356`) already ported
these faithfully as accepted-but-genuinely-inert flags (cgroup v2 has
no realtime-scheduling controller at all — the exact same status both
real reference runtimes themselves have on a v2-only host), but
`ociman` never had a CLI flag reaching the same already-modeled
`LinuxCpu::realtime_period`/`.realtime_runtime` fields at all.

## Real, checked-directly confirmation

`~/git/podman/cmd/podman/common/create.go:939-951`: `cpuRtPeriod
FlagName := "cpu-rt-period"` (`Uint64Var`)/`cpuRtRuntimeFlagName :=
"cpu-rt-runtime"` (`Int64Var`) — real, documented flags on `run`/
`create`/`update` alike (the same `mode != entities.InfraMode` gate
`--cpu-period`/`--cpu-quota`/`--cpu-shares` (`0462`) already use).
Checked directly, again, that this is genuinely inert on cgroup v2:
`~/git/runc/vendor/github.com/opencontainers/cgroups/fs2/cpu.go` has
zero occurrences of `rt_period`/`rt_runtime` at all (only the **v1**
driver, `fs/cpu.go:38-58`, writes `cpu.rt_period_us`/`cpu.rt_
runtime_us`) — confirming this project's own choice to accept-and-
store-but-never-write is the *correct*, faithful port of real
behavior on a cgroup-v2-only host, not a shortcut.

## Implementation

Pure CLI-plumbing onto an already-fully-modeled primitive, the same
shape `0462`/`0463` already established:

- `resources_from_cli` gained two new trailing parameters
  (`cpu_rt_period: Option<u64>`, `cpu_rt_runtime: Option<i64>`),
  added to the "was anything given at all" check and the "should a
  `LinuxCpu` be built at all" check, and set directly onto `LinuxCpu.
  realtime_period`/`.realtime_runtime` — no conversion, no cgroup-
  write logic added anywhere (deliberately: there is nothing to
  write, matching `plan_cpu`'s own already-correct silence on these
  two fields, and `systemd_cgroup`'s own `resource_properties`
  likewise gained no new translation arm for them either).
- `RunArgs` (flattened into both `Command::Run`/`Command::Create`)
  and `Command::Update` (a separate, non-flattened struct) each gain
  `cpu_rt_period: Option<u64>` (`--cpu-rt-period`) and `cpu_rt_
  runtime: Option<i64>` (`--cpu-rt-runtime`, `allow_hyphen_values`
  matching `--cpu-quota`'s own identical signed-integer-flag
  convention), inserted right after `cpu_shares`, before
  `blkio_weight`.
- `synthesize_spec` (shared by `run`/`create`) and `cmd_update` both
  gained the two new parameters, threaded straight through to their
  own already-existing `resources_from_cli` calls.
- `cmd_build`'s own call passes `None, None` — real `podman build`
  has no `--cpu-rt-period`/`--cpu-rt-runtime` of its own at all
  either (checked directly, absent from buildah's own
  `CommonBuildOptions`, the same `run`/`create`/`update`-only status
  `--blkio-weight` (`0463`) already has).
- A stale doc comment fixed as a freebie while in the area: `Command
  ::Build::memory`'s own doc comment still claimed `--cpu-period`/
  `--cpu-quota`/`--cpu-shares` "remain a real, deliberately
  out-of-scope gap" for `ociman build` — false since `0458` actually
  shipped them; corrected to note they (and `--cpuset-cpus`/
  `--cpuset-mems`) are already implemented, and that real `podman
  build` genuinely has no `--cpu-rt-period`/`--cpu-rt-runtime` of its
  own to begin with (unlike the other four, which real `build` also
  lacks but for the "convenience flag" reason, not a realtime-
  scheduling one).

## Tests

Two new unit tests for `resources_from_cli`
(`resources_from_cli_is_some_when_only_a_cpu_rt_flag_is_given`,
`resources_from_cli_carries_cpu_rt_period_and_runtime_verbatim_never_
acted_on`) plus two new integration tests: `run_cpu_rt_flags_are_
accepted_but_set_no_real_systemd_property_at_all` (`tests/tests/
ociman_run.rs` — proves `CPUQuotaPerSecUSec` stays at systemd's own
default `"infinity"` even with both flags given, confirming neither
was ever mistaken for `--cpu-quota`/`--cpu-period` internally) and
`update_cpu_rt_flags_are_accepted_but_write_nothing_to_any_real_
cgroup_file` (`tests/tests/ociman_update.rs` — the same real,
raw-cgroupfs-file verification `ocirun update`'s own identical test
already established, `cpu.max` staying byte-for-byte unchanged). All
111 prior tests in `ociman_run.rs` and all 12 prior tests in
`ociman_update.rs` pass unmodified (112/112 and 13/13 respectively).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (120 test-result
blocks, 0 failures — the first full run hit one transient, known-
flaky failure in `ocicri_container.rs`'s own `create_container_
masked_paths_genuinely_masks_a_real_file_inside_the_running_
container`, exit code 126 "process exited before exec", confirmed
unrelated and passing instantly in isolation; `ci/native-ci.sh`'s own
first attempt hit a second, similarly transient failure in the same
file, `create_container_bind_mount_follows_a_symlinked_host_path`,
also confirmed unrelated and passing instantly in isolation — both
consistent with the already-documented, accepted environmental
flakiness from this dev host's long-running CPU-spinning background
process; both scripts passed clean 120/120 on an immediate retry),
`python3 ci/guards.py`, `cargo deny check`, `bash ci/build-deb.sh`
(real `dpkg -i`/`--version`/`dpkg -r` round trip). This touches `run`/
`create`/`update`'s own real hot path — ran the full `ci/bench.sh`
suite to confirm no regression: all 9 categories show speedups
consistent with previously-recorded baselines.

## Deliberately still out of scope

`--memory-swappiness` and `--oom-kill-disable` remain the other two
real `podman run`/`create`/`update` flags this project has verified,
directly and specifically for this note, to be genuine cgroup-v2 dead
ends: `~/git/runc/vendor/.../cgroups/fs2/memory.go` has zero mentions
of `swappiness` or any OOM-kill-disable write at all (only the v1
driver, `fs/memory.go:131,123`, touches either) — confirming neither
has a real per-cgroup v2 equivalent to accept-and-store the way
`--cpu-rt-period`/`--cpu-rt-runtime` do (there is no `LinuxMemory`
field modeled for either yet, unlike `LinuxCpu::realtime_period`/
`.realtime_runtime`, which already existed from `0356`). Adding
either would need a brand-new spec field with no real host this
project targets ever able to act on it at all — a materially
different, lower-value shape than this increment's "reuse an
already-modeled, already-correctly-inert field" one. `--blkio-weight-
device`/`--device-*-bps`/`--device-*-iops` (per-device block-IO,
`0463`'s own already-documented "still out of scope" note) and
`--cgroup-parent` (a real, separately-scoped, medium-sized gap
touching shared cgroup-path/scope-naming logic, `docs/design/0015`'s
own already-existing "what's still not here" note) remain the two
largest legitimate follow-ups in this same resource-flag area.
