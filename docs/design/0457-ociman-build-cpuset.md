# Design note 0457: `ociman build --cpuset-cpus`/`--cpuset-mems`

Status: implemented
Scope: `bin/ociman/src/build.rs`, `bin/ociman/src/main.rs`,
`tests/tests/ociman_build.rs`.

## What this closes

`ociman build` had no `--cpuset-cpus`/`--cpuset-mems` flag at all —
real `podman build --cpuset-cpus`/`--cpuset-mems`'s way of pinning
every `RUN` step's own cgroup to specific CPUs/NUMA memory nodes,
distinct from (and reusing none of) `ociman run`/`ociman create
--cpuset-cpus`/`--cpuset-mems`'s already-existing per-*container*
flags. Continues the same reuse-the-existing-primitive shape
`0453`-`0456` established, and closes the last *straightforward* part
of the "still ahead" tail `0456` left behind.

## Real, checked-directly confirmation

`~/git/podman/vendor/go.podman.io/buildah/pkg/cli/common.go:435-436`:
`CPUSetCPUs`/`CPUSetMems` are one build-wide value each, no per-stage
or per-instruction variant (same shape as `Ulimit`/`ShmSize`/
`Memory`). `~/git/podman/vendor/go.podman.io/buildah/run_linux.go:
656-661`: `g.SetLinuxResourcesCPUCpus(commonOpts.CPUSetCPUs)`/`g.
SetLinuxResourcesCPUMems(commonOpts.CPUSetMems)` are set in
`addCommonOptsToSpec`, the exact same shared per-`RUN`-invocation
function that also sets `Memory`/`MemorySwap` a few lines below (the
function `0455` already found and wired `LinuxResources` cgroup
support into) — confirming this reuses the exact same `resources`/
`cgroup_setup` machinery `0455` already built, needing zero further
changes to the launch call site itself.

## Implementation

This is the simplest increment in the whole `0453`-`0457` series:
`resources_from_cli` already accepted `cpuset_cpus`/`cpuset_mems`
parameters (build was passing `None` for both since `0455`), and
`cmd_build`'s own `cgroup_setup`-selecting `run_instruction` (built by
`0455`'s real cgroup fix) already keys off whether `resources` is
`Some(...)` at all — no launch-mechanism change needed, unlike
`0455`.

- `cmd_build` gains `cpuset_cpus: Option<&str>`/`cpuset_mems:
  Option<&str>` parameters, passed straight through to the existing
  `resources_from_cli` call (previously `None, None` for these two
  positions).
- `Command::Build` gains `cpuset_cpus: Option<String>`
  (`--cpuset-cpus`) and `cpuset_mems: Option<String>`
  (`--cpuset-mems`), inserted after `http_proxy`, before `quiet` —
  same "no syntax validation at all, straight through to the kernel/
  `systemd_cgroup` translation layer" shape `ociman run/create`'s own
  identical flags already established, including the same known
  rootless-cgroup-cpuset-delegation caveat (`docs/design/0056`).
- A stale doc-comment fix on `0455`'s own `--memory` flag: its
  "`--cpuset-cpus`/`--cpuset-mems` remain a real, deliberately
  out-of-scope gap" note (already corrected once by `0456` to drop
  the nonexistent `--memory-reservation`/`--cpus`) now says these two
  are implemented just below, rather than still pending.

## Tests

One new integration test in `tests/tests/ociman_build.rs`:
`build_cpuset_flags_set_the_real_systemd_scopes_own_allowed_cpus_
property` — the same real, live-property verification `ociman run
--cpuset-cpus`/`--cpuset-mems`'s own test already established, ported
here for `RUN` steps. Unlike `ociman run`'s own version (which reads
the scope name back out of a persisted `state.json`, a real container
concept a `RUN` step doesn't have), this one discovers the transient
`ociman-build-<nonce>.scope` unit by polling `systemctl --user
list-units 'ociman-build-*.scope'` for the one currently-active match
while a long-`sleep`ing `RUN` step keeps it alive — safe since no
other command in this project creates a scope under that prefix.
Also fixed six pre-existing `clippy::unnecessary_qualification`
warnings this file's own newly-added `use std::time::{Duration,
Instant};` import exposed (`cargo fix`, purely cosmetic, no behavior
change — verified by diff). All 133 prior tests in the file pass
unmodified (134/134 total).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (120 test-result
blocks, 0 failures, clean on the first full run), `python3 ci/
guards.py`, `cargo deny check`, `bash ci/native-ci.sh` (one transient,
known-flaky failure on the first attempt — `ociboot_init_mount`'s own
`mount_with_state_dir_assembles_the_writable_view`, confirmed
unrelated to this change, passed in isolation immediately, and the
full script passed clean 120/120 on an immediate retry; the
long-running CPU-spinning process this project's own dev host has
carried since well before this session was still present throughout,
the same accepted, pre-existing environmental flakiness documented
across many earlier increments), `bash ci/build-deb.sh` (real `dpkg
-i`/`--version`/`dpkg -r` round trip). No benchmark re-run needed:
`resources_from_cli` already returns `None` when neither flag is
given, so the no-flag case's cost is provably unchanged from `0455`'s
own already-verified baseline.

## Deliberately still out of scope

`--cpu-period`/`--cpu-quota`/`--cpu-shares` remain the one real,
still-missing piece of buildah's own resource-limit cluster —
unlike `--cpuset-cpus`/`--cpuset-mems`, these would need a small
`resources_from_cli` signature extension first (real `build` exposes
raw period/quota/shares directly, unlike `run`/`create`'s own
`--cpus`-float-to-quota-conversion, which `resources_from_cli`
currently bakes in as its only path to `LinuxCpu`). `NoHosts`/
`NoHostname`/`OmitHistory` (real buildah's own remaining
`CommonBuildOptions` booleans) and `ociman build --volume`
(BuildKit-/buildah-style `RUN --mount=type=bind`) are also still out
of scope.
