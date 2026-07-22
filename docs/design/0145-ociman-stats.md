# Design note 0145: `ociman stats --no-stream`

Status: implemented
Scope: `crates/oci-runtime-core/src/cgroups.rs` (new `cpu_usage_nanos`/
`memory_usage_bytes`/`memory_limit_bytes`/`memory_limit_bytes_clamped_
to_physical_ram`/`pids_current`); `crates/oci-spec-types/src/time.rs`
(new `parse_rfc3339_utc`, the exact inverse of the pre-existing
`format_rfc3339_utc`); `bin/ociman/src/main.rs` (`Command::Stats`,
`cmd_stats`, `ContainerStatsView`, `human_size`);
`tests/tests/ociman_stats.rs` (new, 5 tests).

## A real gap: no resource-usage visibility at all

Every lifecycle command this project has shipped so far (`ps`,
`inspect`, `top`, `pause`/`unpause`) reports a container's own
*identity* and *status*, never its actual, real-time resource
consumption — real `docker stats`/`podman stats` (CPU %, memory
usage/limit, pid count) had no counterpart at all. Picked up as its
own increment rather than folded into a status-display change like
0144, since it needed genuinely new cgroup-accounting plumbing.

## The exact real CPU-percent formula, checked directly

Checked directly against real podman's own source
(`~/git/podman/libpod/stats_linux.go`'s `calculateCPUPercent`/
`getPlatformContainerStats`, and `~/git/podman/libpod/stats_common.go`'s
`GetContainerStats`): `cpu_percent = (cpu_delta_ns / wall_clock_delta_
ns) * 100`, where `cpu_delta_ns` is the difference between two samples
of the cgroup's own cumulative CPU-nanosecond counter, and
`wall_clock_delta_ns` is the real wall-clock time between those two
samples — **not** normalized by CPU count, so a single-core-saturating
container legitimately reports `~100%`, not `~100%/nproc`, matching
real observed `podman stats` output exactly (confirmed empirically
against a real `podman run -d` container running a genuine busy loop:
`100.11%`).

The interesting, checked-directly subtlety: for the very *first*
sample of a container's life (`--no-stream`'s own only mode, see
below) real podman has no previous sample to diff against at all —
`GetContainerStats(nil)` handles this by using `0` as the previous CPU
value and the container's own separately tracked `StartedTime` as the
previous wall-clock value (`stats_common.go`'s own `GetContainerStats`).
The formula becomes exactly `(total CPU-ns consumed so far) / (wall-
clock ns elapsed since the container started) * 100` — an *average*
CPU percentage over the container's whole life so far, not an
instantaneous one. This project has no separate `StartedTime` field of
its own (`PersistedState` only records `created`); for `ociman run`
(the only way to start a container at all right now — see "what this
doesn't do yet" below) `created` and "started" are, for all practical
purposes, the same instant, so `created` stands in for it directly.
`oci_spec_types::time::parse_rfc3339_utc`, the exact inverse of the
pre-existing `format_rfc3339_utc`, was added to parse it back into a
real `SystemTime` for the elapsed-time computation (hand-rolled, no new
date/time dependency, same reasoning as `format_rfc3339_utc`'s own doc
comment: Howard Hinnant's own published `days_from_civil` is the exact
inverse of the formatter's already-in-tree `civil_from_days`).

## New cgroup v2 accounting reads, checked directly against real podman's own cgroup library

Four new functions in `oci_runtime_core::cgroups`, each checked
directly against `~/git/container-libs/common/pkg/cgroups/{cpu,memory}
_linux.go` (the real cgroup-accounting library real podman itself
uses) rather than inferred from the kernel docs alone:

* `cpu_usage_nanos`: `cpu.stat`'s own `usage_usec` key, `* 1000` for
  nanoseconds — matches `cpuStat` exactly.
* `memory_usage_bytes`: `memory.current` minus `memory.stat`'s own
  `inactive_file` (clamped to zero) — matches `memoryStat` exactly,
  the same "reclaimable page cache isn't real usage" docker convention
  podman itself ports, not the raw (and much less useful)
  `memory.current` alone.
* `memory_limit_bytes`: `memory.max`, with the kernel's own `"max"`
  sentinel mapped to `u64::MAX` — matches `readFileAsUint64` exactly.
* `memory_limit_bytes_clamped_to_physical_ram`: `memory_limit_bytes`,
  clamped to this host's own real total RAM (`rustix::system::
  sysinfo().totalram`, already available workspace-wide — the
  `"system"` rustix feature turned out to already be enabled at the
  `[workspace.dependencies]` level, no `Cargo.toml` edit needed at
  all) whenever the cgroup itself reports no limit or one larger than
  physical RAM — matches real podman's own `getMemLimit` exactly,
  including its own real, checked-directly quirk of using
  `Sysinfo.Totalram` completely unscaled by its own `mem_unit` field
  (correct on every mainstream 64-bit Linux target, where `mem_unit`
  is always `1`).
* `pids_current`: a bare `pids.current` read.

All four (plus the two small private helpers they share,
`read_single_value_as_u64`/`read_stat_key_as_u64`) are unit-tested
against plain temp-directory files standing in for real cgroupfs
(same technique the pre-existing freezer tests already use) — no real
privilege needed for any of it.

## `ociman stats <id> --no-stream [--json]`

Resolves the container's own real, current cgroup the exact same way
`cmd_top`/`cmd_pause`/`cmd_unpause` already do
(`cgroup_dir_for_running_pid`, since `ociman`'s own containers never
have a persisted `cgroupsPath` — see 0143's own finding), reads all
four accounting values, computes `cpu_percent`/`mem_percent`, and
prints either the same "one JSON object" shape `inspect --json` uses,
or a table with the real, matching column headers (`CPU %`, `MEM
USAGE / LIMIT`, `MEM %`, `PIDS`, ...).

`human_size` approximates real docker/podman's own `go-units`
`HumanSize` (checked directly against
`~/git/moby/vendor/github.com/docker/go-units/size.go`): same
base-1000 units and roughly the same 4-significant-digit precision —
every real observed example checked (`110B`, `430B`, `65.54kB`,
`128.5GB`, plus `go-units`'s own doc-comment examples `796kB`/
`2.746MB`) matches exactly, though it isn't a byte-for-byte port of
Go's own `%.4g` float formatter for every possible input (see "what
this doesn't do yet").

## `--no-stream` is required — real streaming mode is a clear, loud error, not a silent difference

Real `podman stats`'s own *default* behavior (no `--no-stream`)
streams continuously, re-sampling roughly once a second until
interrupted (`pkg/domain/infra/abi/containers.go`'s own
`ContainerStats`, `goto stream` loop). Not implemented yet — rather
than silently behaving differently (e.g. printing once and exiting
anyway, or hanging forever with no way to test it), bare `ociman
stats <id>` is a clear, loud, immediate error telling the caller to
pass `--no-stream`, matching this project's own already-established
"loud error over silently-wrong behavior" convention.

## Real, automated tests

Five new integration tests in `tests/tests/ociman_stats.rs`: a real,
running, genuinely CPU-burning container (same technique
`ociman_pause.rs` already established) reports a substantial CPU %
(`> 50.0`) and non-zero memory usage via `--json`; the plain-table
output contains the real expected column headers and the container's
own id; bare `ociman stats <id>` (no `--no-stream`) is a clear error
mentioning `--no-stream`; `stats` against an already-stopped container
is a clear error; against an unknown container id too.

One real, empirically-found tuning detail worth recording: the first
version of the CPU-percent test used only a 500ms burn before
sampling and asserted `> 50.0`, and flaked at `~33%` — not a bug, but
the real, fixed per-container setup overhead (image/rootfs/cgroup/
systemd-scope creation, all counted in the "elapsed since `created`"
denominator despite contributing zero CPU usage of their own) diluting
the ratio measurably at that short an interval. Fixed by burning for a
full 3 real seconds instead, long enough for that fixed overhead to
stop dominating — confirmed stable across repeated runs.

Full `cargo build --workspace --locked`/`cargo test --workspace
--locked` (2 clean runs)/`cargo fmt --all --check`/`cargo clippy
--workspace --all-targets --locked -- -D warnings`/`python3
ci/guards.py`/`cargo deny check`/`bash ci/native-ci.sh` all clean.

## What this doesn't do yet

* No continuous streaming mode (real `podman stats`'s own *default*
  behavior) — `--no-stream` is required, see above.
* No `--all`/multiple-container support, `--format` beyond plain
  table/`--json`, or `NET IO`/`BLOCK IO` columns (this project has no
  network-namespace setup of its own yet, so "net I/O" would be
  meaningless; block I/O accounting was left out of this narrow first
  increment, not fundamentally harder to add later).
* `human_size` is a close approximation of real Go `go-units`
  `HumanSize`'s own `%.4g` float formatting, not a byte-for-byte port —
  every real example checked matches, but an adversarial input
  designed to hit Go's own specific floating-point rounding edge cases
  might format very slightly differently.
* `cpu_percent`'s very-first-sample formula uses this project's own
  `created` timestamp as a stand-in for real podman's separately
  tracked container-start time; for a combined `ociman run` (this
  project's only way to start a container right now) these are the
  same instant in practice, but would diverge if a future increment
  ever adds a separate `create` + `start` two-step flow for `ociman`
  itself (matching `ocirun`'s own existing separate `create`/`start`).
