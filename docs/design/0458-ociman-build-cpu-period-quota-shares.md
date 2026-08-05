# Design note 0458: `ociman build --cpu-period`/`--cpu-quota`/`--cpu-shares`

Status: implemented
Scope: `bin/ociman/src/build.rs`, `bin/ociman/src/main.rs`,
`tests/tests/ociman_build.rs`.

## What this closes

`ociman build` had no `--cpu-period`/`--cpu-quota`/`--cpu-shares`
flag at all — real `podman build`'s own raw CPU CFS controls, the
last real, still-missing piece of buildah's own resource-limit
cluster this series (`0453`-`0457`) hadn't closed yet. Unlike
`ociman run`/`create`'s own `--cpus` (a convenience float that
`resources_from_cli` converts into a quota/period pair itself), real
`podman build` exposes the raw values directly with no `--cpus`
equivalent of its own at all (confirmed directly, see `0456`'s own
doc comment).

## Real, checked-directly confirmation

`~/git/podman/vendor/go.podman.io/buildah/pkg/cli/common.go:432-434`:
`fs.Uint64Var(&flags.CPUPeriod, "cpu-period", 0, ...)`/`fs.
Int64Var(&flags.CPUQuota, "cpu-quota", 0, ...)`/`fs.Uint64VarP(&flags.
CPUShares, "cpu-shares", "c", 0, ...)` — one build-wide value each, no
per-stage variant (same shape as every other flag in this series).
`~/git/podman/vendor/go.podman.io/buildah/run_linux.go:647-661`'s own
`addCommonOptsToSpec`: `g.SetLinuxResourcesCPUPeriod(commonOpts.
CPUPeriod)`/`g.SetLinuxResourcesCPUQuota(commonOpts.CPUQuota)`/`g.
SetLinuxResourcesCPUShares(commonOpts.CPUShares)` are set in the
exact same function `0457` already found setting `CPUSetCPUs`/
`CPUSetMems` a few lines below — confirming this reuses the same
`resources`/`cgroup_setup` machinery `0455` built, with zero further
launch-mechanism changes needed (same as `0457`).

## Implementation

Unlike `0457` (a pure passthrough needing no signature changes at
all), `resources_from_cli` itself needed extending: it previously had
no way to carry a *raw* period/quota/shares value at all, only ever
computing them from `--cpus`. Extended with three new trailing
parameters (`cpu_period: Option<u64>`, `cpu_quota: Option<i64>`,
`cpu_shares: Option<u64>`) rather than writing a second, parallel
`LinuxCpu` builder:

- `quota`/`period` are now `cpu_quota.or_else(|| cpus.map(...))`/
  `cpu_period.or_else(|| cpus.map(...))` — an explicit raw value
  always wins over a `cpus`-derived one (documented, though the two
  never actually collide in practice today: `run`/`create`/`update`
  only ever pass `cpus`, `build` only ever passes the raw three).
- `shares` is now `cpu_shares` directly (`LinuxCpu.shares` was already
  modeled and already reaches `systemd_cgroup`'s own `CPUWeight`
  translation — simply never reachable from any CLI flag before this
  increment, in *any* of this project's own commands, not just
  `build`).
- The "was anything given at all" early-return check, and the "should
  a `LinuxCpu` be built at all" check just below it, both gained the
  three new conditions.
- Both existing call sites (`prepare_container`, shared by `run`/
  `create`; `cmd_update`) now pass `None, None, None` for the three
  new positions — real `run`/`create`/`update` have no raw
  period/quota/shares flags of their own (an existing, separately-
  scoped gap noted directly in the new doc comment: `LinuxCpu.shares`
  reaching a CLI flag for the *first* time here, via `build` only).
  All 15 existing `resources_from_cli` unit tests updated to the new
  10-argument signature (mechanical, no test logic changed).
- `cmd_build` gains `cpu_period: Option<u64>`/`cpu_quota:
  Option<i64>`/`cpu_shares: Option<u64>` parameters, passed straight
  through to the extended `resources_from_cli` call (previously
  `cpuset_cpus, cpuset_mems` were the last two arguments).
- `Command::Build` gains `cpu_period: Option<u64>` (`--cpu-period`),
  `cpu_quota: Option<i64>` (`--cpu-quota`), and `cpu_shares:
  Option<u64>` (`--cpu-shares`/`-c`, matching real podman build's own
  short flag), inserted after `cpuset_mems`, before `http_proxy`.

## Tests

Four new unit tests for `resources_from_cli` itself
(`resources_from_cli_is_some_when_only_a_raw_cpu_flag_is_given`,
`resources_from_cli_carries_raw_cpu_period_quota_and_shares_
verbatim`, `resources_from_cli_prefers_an_explicit_cpu_quota_and_
period_over_a_cpus_derived_one`) plus one new integration test in
`tests/tests/ociman_build.rs`
(`build_cpu_period_quota_and_shares_set_the_real_systemd_scopes_own_
properties`) — the same real, live-property `systemctl --user show`
verification `0457`'s own cpuset test established, using the exact
same `--cpu-period 100000 --cpu-quota 150000` (1.5 CPUs over 100ms)
numbers `ociman run --cpus 1.5`'s own test already confirmed render
as `CPUQuotaPerSecUSec` `1.500000s`, plus `--cpu-shares 1024`
(confirmed directly, `systemd_cgroup.rs`'s own unit tests, to
translate to `CPUWeight` `100`).

**A real test-isolation bug found and fixed along the way**: this new
test and `0457`'s own cpuset test both discover their live scope by
*pattern* (`ociman-build-*.scope`, since a `RUN` step's transient
scope name has no persisted record to look up exactly, unlike `run`/
`create`'s own `state.json`-keyed lookup) — running concurrently
(`cargo test`'s own default), each could observe the *other* test's
scope instead of its own, a real, previously-latent race the very
first attempt at this new test caught directly (a flaky `AllowedCPUs`
mismatch in the *other*, already-passing test). Fixed with a real,
process-wide blocking `flock` (`lock_build_scope_tests`, reusing
`ociboot_build_image.rs`'s own already-established `rustix::fs::flock`
pattern, a plain blocking lock here rather than that fixture's own
non-blocking one) serializing every `wait_for_build_scope` caller in
the file against every other one — confirmed fixed by three
consecutive clean full-file runs after the fix, where the very first
attempt without it reproduced the race immediately.

All 134 prior tests in the file pass unmodified (135/135 total after
adding one net new integration test — `0457`'s own new cpuset test
already having landed in the prior increment).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (120 test-result
blocks, 0 failures, clean on the first full run), `python3 ci/
guards.py`, `cargo deny check`, `bash ci/native-ci.sh` (clean, 120/120,
clean on the first run too), `bash ci/build-deb.sh` (real `dpkg -i`/
`--version`/`dpkg -r` round trip). No benchmark re-run needed: the
no-flag case's cost is unchanged from `0455`'s own already-verified
baseline (`resources_from_cli` still returns `None` when nothing at
all is given).

## Deliberately still out of scope

This closes the entire real resource-limit tail `0453` first started
tracking (`--ulimit`/`--shm-size`/`--memory`/`--memory-swap`/
`--cpuset-cpus`/`--cpuset-mems`/`--cpu-period`/`--cpu-quota`/
`--cpu-shares`) — every one of buildah's own `CommonBuildOptions`
resource-shaped fields now has a working `ociman build` flag.
`NoHosts`/`NoHostname`/`OmitHistory` (real buildah's own remaining
non-resource `CommonBuildOptions` booleans) and `ociman build
--volume` (BuildKit-/buildah-style `RUN --mount=type=bind`, a larger,
differently-shaped gap) remain the natural next candidates in this
same file.
