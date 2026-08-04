# Design note 0401: `--memory-reservation` (`ociman run/create/update`, `ocirun update`)

Status: implemented
Scope: `bin/ociman/src/main.rs`, `bin/ocirun/src/main.rs`,
`tests/tests/ociman_run.rs`, `tests/tests/ociman_update.rs`,
`tests/tests/ocirun_update.rs`, `README.md`.

## What this closes

Real `docker run --memory-reservation`/`podman run
--memory-reservation` and real `runc update --memory-reservation`/
`crun update --memory-reservation` — a soft memory limit distinct from
`--memory`'s own hard cap — had no flag anywhere in this project,
despite the underlying spec field and both cgroup drivers already
fully supporting it: `oci_spec_types::runtime::LinuxMemory.
reservation: Option<i64>` already existed, already read by
`oci_runtime_core::cgroups`' own raw-cgroupfs writer (`memory.low`)
and by `systemd_cgroup`'s own `MemoryLow` D-Bus property translation —
neither of which had a CLI flag reaching them before now. Previously
named explicitly as out of scope in both `Command::Update`'s own doc
comment (`ociman`) and this project's own general "not supported"
lists.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/common/create.go`'s own
  `memoryReservationFlagName` — a plain `--memory-reservation`,
  parsed the same `RAMInBytes` grammar every other memory flag uses,
  with zero CLI-level validation against `--memory` (unlike
  `--memory-swap`, which real docker's own `verifyPlatformContainerResources`
  does check).
- `~/git/runc/update.go`'s own `MemoryReservation` and
  `~/git/crun/src/update.c`'s own `"memory-reservation"` table entry —
  both confirm `ocirun update` was the one real, remaining gap in an
  otherwise-complete ad-hoc update flag set (every other real
  `runc`/`crun update` flag — `memory`/`memory-swap`/`pids-limit`/
  `cpuset-*`/`cpu-share`/`cpu-period`/`cpu-quota`/`cpu-burst`/
  `cpu-idle`/`cpu-rt-period`/`cpu-rt-runtime`/`blkio-weight` — was
  already present).

## Implementation

- `ociman`: `--memory-reservation` added to `RunArgs` (mirrors
  `--memory`'s own doc comment shape) and `Command::Update`.
  `parse_and_validate_memory_and_cpus` gains a third parameter,
  parsed via `parse_memory_limit` (reused verbatim — the identical
  `RAMInBytes` grammar backs `--memory`/`--memory-reservation`/
  `--shm-size` alike), with **no** relationship check against
  `--memory` at all, matching real docker/podman's own identical
  zero-validation-beyond-parsing behavior. `resources_from_cli`
  restructured so `LinuxMemory` is built whenever *either* `--memory`
  or `--memory-reservation` is given (not just the former) — a bare
  reservation with no hard limit is a real, meaningful request on its
  own, the same "built from any one of several related flags" shape
  `LinuxCpu` already establishes for `--cpus`/`--cpuset-cpus`/
  `--cpuset-mems`. The existing swap-defaulting logic (`--memory` with
  no explicit `--memory-swap` defaults to double) only fires when
  `--memory` itself was given — a bare `--memory-reservation` has no
  hard limit to double.
- `ocirun`: `--memory-reservation` added to `Command::Update` and
  `UpdateFlags`; `resources_from_flags`'s existing `mem` builder gains
  one more `if let Some(...)` arm, following the exact same in-place
  mutation shape `memory`/`memory_swap` already use.
- `cmd_update` (`ociman`) needed `#[allow(clippy::too_many_arguments)]`
  once its parameter count crossed clippy's default threshold (8),
  matching the same attribute several other multi-flag functions in
  this file already carry.

## Tests

`ociman`: 6 new/updated unit tests for `parse_and_validate_memory_and_
cpus`/`resources_from_cli` (independent parsing, no relationship
check with `--memory`, a bare reservation producing a real
`LinuxMemory` with `limit: None`/`swap: None`). One new end-to-end
integration test, `run_memory_reservation_flag_sets_the_real_systemd_
scopes_own_memory_low` (mirrors the existing `--memory-swap` test's
own `systemctl --user show ... MemoryLow` technique), given with no
`--memory` at all. `ociman update`'s own existing live-cgroup test
extended with `--memory-reservation`, checking the real `memory.low`
file directly.

`ocirun`: 1 new unit test, `resources_from_flags_builds_memory_
reservation_alone_and_combined`. One new end-to-end integration test,
`update_ad_hoc_memory_reservation_flag_writes_the_real_cgroup`
(`memory.low` read back directly, alongside `--memory`'s own
`memory.max`).

All existing tests continue to pass unmodified.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures), `python3
ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh`, `bash
ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg -r` round trip).
This touches `resources_from_cli`/`synthesize_spec`, both on
`ociman run`'s own hot create path, so `ci/bench.sh` was re-run: every
figure held at or improved on its own recorded baseline (`ocirun run`
2.12×/6.34× faster than crun/runc; `ociman run --rm` 5.22×/7.56×
faster than podman/docker; `ociman rm` 42.97× faster; `ociman run -d`
3.59×/4.24× faster than podman/docker) — the added parameter/field is
a plain `Option` check on every measured path, no new syscalls or
allocations for a caller not using this new, opt-in flag.

## Deliberately still out of scope

`--memory-swappiness` (cgroup v2 has no per-cgroup `memory.swappiness`
equivalent at all — would only ever be a documented, inert no-op, the
same class of gap `ocirun update --cpu-rt-period`/`--cpu-rt-runtime`
already established) and `--device`/`--device-*-bps`/`--device-*-iops`
(need a new `LinuxDevice` spec type distinct from the existing
access-rule-only `LinuxDeviceCgroup`, plus host-device resolution — a
materially bigger feature) remain unimplemented, matching this
project's own already-documented scope limits.
