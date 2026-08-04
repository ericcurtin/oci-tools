# Design note 0455: `ociman build --memory`/`--memory-swap`

Status: implemented
Scope: `bin/ociman/src/build.rs`, `bin/ociman/src/main.rs`,
`tests/tests/ociman_build.rs`.

## What this closes

`ociman build` had no `--memory`/`--memory-swap` flag at all — real
`podman build --memory`/`-m`/`--memory-swap`'s way of capping every
`RUN` step's own cgroup memory usage, distinct from (and reusing none
of) `ociman run`/`ociman create --memory`'s already-existing
per-*container* flag. Continues the same reuse-the-existing-primitive
shape `0453`/`0454` (`--ulimit`/`--shm-size`) established.

## Real, checked-directly confirmation

`~/git/podman/vendor/go.podman.io/buildah/pkg/cli/common.go:444-445`:
`Memory`/`MemorySwap` are one build-wide value each, no per-stage or
per-instruction variant (same shape as `Ulimit`/`ShmSize`).
`~/git/podman/vendor/go.podman.io/buildah/run_linux.go:665-669`:
`g.SetLinuxResourcesMemoryLimit(commonOpts.Memory)`/`g.
SetLinuxResourcesMemorySwap(commonOpts.MemorySwap)` are set in the
same shared per-`RUN`-invocation function `0453`/`0454` already found
calling `addRlimits`/`setupSpecialMountSpecChanges` — confirming this
applies fresh to every `RUN` step in every stage, not just once.

## A real gap found while wiring this up (not just plumbing, unlike
   0453/0454)

Unlike `--ulimit` (a per-process `setrlimit(2)` syscall) and
`--shm-size` (a mount option), a memory limit is a **cgroup**
concept — it needs a real cgroup to write into. `ociman build`'s own
`RUN`-step launch call (`oci_runtime_core::launch::run`) has always
used `CgroupSetup::FromSpec` (the same spec-driven, raw-cgroupfs mode
`ocirun` itself uses), which only ever writes real limit files when
`bundle.spec.linux.cgroupsPath` is set — and `run_step_spec` has never
set one, since no `RUN` step has ever needed a real cgroup before now.
Simply setting `linux.resources` in the spec (as `0453`/`0454` did for
`rlimits`/the `/dev/shm` mount) would have silently done nothing at
all — caught directly by this increment's own first test attempt,
which completed successfully instead of getting OOM-killed as
expected.

Fixed by giving a `RUN` step the exact same treatment `ociman run`/
`create` already get (see `cmd_run`'s own `cgroup_setup`
construction, `docs/design/0033`/`0034`/`0037`): a transient systemd
scope (`ociman-build-<nonce>.scope`), with `resources` translated
into systemd unit properties via the same `systemd_cgroup` module —
but **only** when `resources` is actually `Some(...)` (i.e.
`--memory`/`--memory-swap` was actually given). `run_instruction` now
calls `oci_runtime_core::launch::run_reporting_pid` directly (rather
than the thinner `run` wrapper, which always hardcodes `CgroupSetup::
FromSpec`) with a `cgroup_setup` chosen by a `match` on `resources`:
`FromSpec` (functionally identical to every earlier `RUN` step's own
call, since `run` itself is just `run_reporting_pid` with that same
mode and a no-op `on_pid` callback) when `None`, `Systemd { ... }`
when `Some`. This means the overwhelmingly common no-`--memory`-flag
case is completely unchanged in cost or behavior — verified directly
by `ci/bench.sh`'s `build --no-cache`/`build (cached)` sections
showing no regression (see below).

## Implementation

- Reused `ociman run`/`create --memory`/`--memory-swap`'s existing
  `parse_and_validate_memory_and_cpus` (validation: `--memory-swap`
  requires `--memory`, must be at least as large) and `resources_
  from_cli` (spec construction) verbatim via `crate::` paths — no new
  parser, no new validation, no new `LinuxResources` builder. Called
  once per build invocation in `cmd_build`, passing `None` for
  `--memory-reservation`/`--cpus` (no `build` counterpart yet for
  either — out of scope for this increment).
- `StageContext<'a>` gains a new `resources: &'a Option<LinuxResources>`
  field (a reference, not an owned clone, avoiding a real clone per
  stage the way `rlimits`'s own `&'a [PosixRlimit]` already does),
  carried the same way `rlimits`/`shm_size_bytes` already are.
- `apply_instruction`'s `Instruction::Run` arm passes `stage_ctx.
  resources` through to `run_instruction`, which threads it to `run_
  step_spec`'s new trailing `resources: &Option<LinuxResources>`
  parameter; its body sets `linux.resources = resources.clone()` —
  the exact same field `main.rs`'s `synthesize_spec` already sets for
  `run`/`create`.
- `run_instruction`'s own launch call site (see above) now builds a
  `cgroup_setup` from `resources` and calls `run_reporting_pid`
  directly instead of the thinner `run` wrapper.
- `Command::Build` gains `memory: Option<String>` (`-m`/`--memory`,
  matching real podman build's own `-m` short flag — checked
  directly, no existing short-flag collision in `Command::Build`) and
  `memory_swap: Option<String>` (`--memory-swap`, `allow_hyphen_values`
  for the same `-1`-means-unlimited reason `run`/`create`'s own flag
  needs it), inserted after `shm_size`, before `quiet`.

## Tests

Two new tests in `tests/tests/ociman_build.rs`:
`build_memory_limit_actually_gets_enforced_by_the_kernels_own_oom_
killer` (a real, kernel-enforced verification — a `RUN` step under a
real 16 MiB `--memory` limit allocating ~300 MB genuinely gets killed
by the kernel's own cgroup v2 OOM killer, failing the build; the same
pattern already established by `ociman run --memory`'s own test) and
`build_memory_swap_without_memory_is_a_clear_error` (the reused
validation surfaces correctly from `build` too). All 128 prior tests
in the file pass unmodified (130/130 total).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (120 test-result
blocks, 0 failures, clean on the first full run), `python3 ci/
guards.py`, `cargo deny check`, `bash ci/native-ci.sh` (clean,
120/120, clean on the first run too), `bash ci/build-deb.sh` (real
`dpkg -i`/`--version`/`dpkg -r` round trip). Unlike `0453`/`0454`,
this one genuinely touches the `RUN`-step launch call site itself
(not just the spec passed into it) — ran the full `ci/bench.sh` suite
to confirm: `build --no-cache` 15.47x faster than docker/20.99x
faster than podman, `build (cached)` 21.68x faster than podman/27.41x
faster than docker, both consistent with this project's
previously-recorded baselines, no regression (the no-`--memory`-flag
path is provably unchanged, see above).

## Deliberately still out of scope

`--memory-reservation`, `--cpus`, `--cpuset-cpus`, `--cpuset-mems`
(real buildah's own `CommonBuildOptions.CPUPeriod`/`CPUQuota`/
`CPUShares`/`CPUSetCPUs`/`CPUSetMems`, applied in the exact same
shared per-`RUN`-invocation function `--memory`/`--memory-swap` are)
would all reuse the same `resources_from_cli`/`cgroup_setup` plumbing
this increment just built, and are natural, small follow-ups —
deferred to their own separate increments rather than one larger
combined change. `ociman build --volume` (BuildKit-/buildah-style
`RUN --mount=type=bind`) remains a larger, differently-shaped gap.
