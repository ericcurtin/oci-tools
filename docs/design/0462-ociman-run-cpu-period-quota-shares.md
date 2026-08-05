# Design note 0462: `ociman run`/`create`/`update --cpu-period`/`--cpu-quota`/`--cpu-shares`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_run.rs`, `tests/
tests/ociman_update.rs`.

## What this closes

`ociman run`/`create`/`update` had no `--cpu-period`/`--cpu-quota`/
`--cpu-shares` flags of their own at all — real `docker run`/`podman
run`/`create`/`update`'s own raw CPU CFS controls, distinct from (and
previously never reaching) the exact same three parameters
`resources_from_cli` already gained for `ociman build` alone in
`0458`. This was a real, previously-unnoticed, checked-directly gap:
this project's own `Command::Update` doc comment had claimed these
three were "out of scope for the same reason `run` itself doesn't
support them either" — a claim that (checked directly against real
podman's own docs/source, not assumed) turned out to be simply wrong
for all three of `run`/`create`/`update` alike.

## Real, checked-directly confirmation

`~/git/podman/docs/source/markdown/podman-run.1.md.in:124,126,132`,
`podman-create.1.md.in:105,107,113`, and `podman-update.1.md.in:21,23,
29` each include `@@option cpu-period`/`cpu-quota`/`cpu-shares` — all
three real, documented flags on all three real subcommands.
`~/git/podman/cmd/podman/common/create.go:923-937,1020-1026`:
`cpuPeriodFlagName`/`cpuQuotaFlagName` are registered whenever `mode
!= entities.InfraMode` (true for `entities.CreateMode`/`RunMode`/
`UpdateMode` alike, only excluded for pod infra containers), and
`cpuSharesFlagName` (`"cpu-shares"`, short `"c"`) unconditionally.
`~/git/podman/cmd/podman/containers/update.go:53`: `common.
DefineCreateFlags(cmd, &updateOptions.ContainerCreateOptions,
entities.UpdateMode)` — `update` shares the exact same flag-
registration function `create`/`run` do, confirming all three
genuinely get the same three flags.

## Implementation

No new logic needed at all — `resources_from_cli`'s own raw
`cpu_period`/`cpu_quota`/`cpu_shares` parameters, and the `systemd_
cgroup`/raw-cgroupfs translation layers underneath them (`CPUWeight`
from `cpu.shares`, `CPUQuotaPerSecUSec`/`CPUQuotaPeriodUSec` or
`cpu.max`/`cpu.weight` from `cpu.quota`/`.period`, depending on
whether the caller is a live systemd-scope container or a raw-
cgroupfs `update`), were already fully built and tested by `0458` for
`ociman build`. This was purely a CLI-flag-definition-and-threading
gap:

- `RunArgs` (`#[command(flatten)]`'d into both `Command::Run`/
  `Command::Create`, so this one edit covers both at once) gains
  `cpu_period: Option<u64>` (`--cpu-period`), `cpu_quota:
  Option<i64>` (`--cpu-quota`), and `cpu_shares: Option<u64>`
  (`--cpu-shares`/`-c`), inserted right after `cpuset_mems`.
- `Command::Update` gains the identical three fields (a separate,
  non-flattened struct, so needed its own copy — matching how
  `cpuset_cpus`/`cpuset_mems` etc. are already duplicated there too).
- `synthesize_spec` (shared by `run`/`create` via `prepare_container`)
  and `cmd_update` both gained the three new parameters, threaded
  straight through to their own already-existing `resources_from_cli`
  calls (previously hardcoding `None, None, None` for these three
  positions, now passing the real values).
- Two stale doc comments fixed: `Command::Update`'s own "out of
  scope" claim (corrected to reflect these three are now supported,
  narrowing the still-genuinely-out-of-scope list to `--cpu-rt-
  period`/`--cpu-rt-runtime`/`--memory-swappiness`/`--blkio-weight*`/
  `--device-*-bps`/`--device-*-iops`), and `resources_from_cli`'s own
  doc comment (previously claimed `run`/`create`/`update` "only ever
  pass `cpus`" — no longer true, corrected to describe the real,
  now-reachable `--cpus`-and-raw-value combination explicitly).

## Tests

Two new integration tests: `run_cpu_period_quota_and_shares_set_the_
real_systemd_scopes_own_properties` (`tests/tests/ociman_run.rs`,
the same real, live `systemctl --user show` verification technique
`run_cpus_flag_sets_the_real_systemd_scopes_own_cpu_quota` already
established, reusing the exact same `--cpu-period 100000 --cpu-quota
150000`/`--cpu-shares 1024` numbers `0458`'s own build-side test
already confirmed render as `CPUQuotaPerSecUSec` `1.500000s`/
`CPUWeight` `100`) and `update_cpu_period_quota_and_shares_writes_the_
real_cgroup_files` (`tests/tests/ociman_update.rs`, the same real,
raw-cgroupfs-file verification `update_changes_the_real_live_cgroup_
limits_of_a_running_container`'s own `--cpus`/`cpu.max` check already
established, for `cpu.max`/`cpu.weight` directly). All 108 prior tests
in `ociman_run.rs` and all 10 prior tests in `ociman_update.rs` pass
unmodified (109/109 and 11/11 respectively); all 7 `ociman_create.rs`
tests pass unmodified too (confirming `RunArgs`'s shared addition
didn't disturb `create`'s own behavior).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (120 test-result
blocks, 0 failures, clean on the first full run), `python3 ci/
guards.py`, `cargo deny check`, `bash ci/native-ci.sh` (clean,
120/120, clean on the first run too), `bash ci/build-deb.sh` (real
`dpkg -i`/`--version`/`dpkg -r` round trip). This touches `run`/
`create`/`update`'s own real hot path (`synthesize_spec`'s own spec-
construction, directly measured by `ci/bench.sh`) — ran the full
suite to confirm no regression: all 9 categories show speedups
consistent with previously-recorded baselines (`run --rm` 5.85x/
`run -d` 3.62x/`rm` 45.38x/`commit` 39.70x faster than podman, `build
--no-cache` 17.98x faster than docker, `build` (cached) 23.35x faster
than podman), the three new always-`None`-by-default parameters
adding no measurable cost.

## Deliberately still out of scope

Real `podman update`'s own remaining, genuinely still-missing flags:
`--cpu-rt-period`/`--cpu-rt-runtime` (cgroup v2 has no real-time
scheduling controller at all — accepted on parse, never acted on,
matching `LinuxCpu.realtime_runtime`/`.realtime_period`'s own already-
documented status), `--memory-swappiness` (a permanent, deliberate
no-op, `docs/design/0401`), and `--blkio-weight*`/`--device-*-bps`/
`--device-*-iops` (block-IO controls — `ocirun update --blkio-weight`
already has the underlying `LinuxBlockIo`/`plan_blkio` primitive
built and unit-tested, `docs/design/0366`, but it has never been
exposed through `ociman` at all, and `systemd_cgroup`'s own `resource_
properties` has no `IOWeight`/block-IO translation arm yet either — a
natural, similarly-shaped follow-up to this same increment).
