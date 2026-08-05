# Design note 0463: `ociman run`/`create`/`update --blkio-weight`

Status: implemented
Scope: `crates/oci-runtime-core/src/systemd_cgroup.rs`, `bin/ociman/
src/main.rs`, `bin/ociman/src/build.rs`, `tests/tests/ociman_run.rs`.

## What this closes

`ociman run`/`create`/`update` had no `--blkio-weight` flag of their
own at all — real `docker run`/`podman run`/`create`/`update`'s own
relative block-IO weight control. The underlying `LinuxBlockIo`
primitive (`oci_spec_types::runtime::LinuxBlockIo`) and its raw-
cgroupfs driver (`oci_runtime_core::cgroups::plan_blkio`/`apply`,
including the real BFQ-weight-to-`io.weight` conversion,
`convert_blkio_weight_to_io_weight`) were already fully built and
unit-tested for `ocirun update --blkio-weight` (`0366`) — but never
reachable from any `ociman` CLI flag, and `systemd_cgroup`'s own
resource-property translation (used by `ociman run`/`create`'s own
systemd-scope containers, a completely different code path from
`ocirun`'s raw cgroupfs one) had no `block_io`/`IOWeight` arm at all.

## Real, checked-directly confirmation

`~/git/podman/cmd/podman/common/create.go:1060-1066`:
`blkioWeightFlagName := "blkio-weight"`, a real, documented flag
(string-typed in Go purely so an empty string means "not given"; the
value itself is still parsed as a plain `uint16` immediately after,
`~/git/podman/pkg/specgenutil/specgen.go:84-93`'s own `strconv.
ParseUint(b, 10, 16)` — no extra validation beyond that, matched here
by using a plain `Option<u16>` clap flag directly, the same
zero-extra-validation stance this project's own `--cpuset-cpus`/
`--cpuset-mems` already established). `~/git/podman/docs/source/
markdown/podman-update.1.md.in:17`: `@@option blkio-weight` — real on
`update` too. `~/git/crun/src/libcrun/cgroup-systemd.c`'s own
`append_io_weight`: `weight = IO_WEIGHT(resources->block_io->weight);
... sd_bus_message_append(m, "(sv)", "IOWeight", "t", weight)` — the
exact same `IO_WEIGHT` BFQ-to-`io.weight` conversion this crate's own
`convert_blkio_weight_to_io_weight` already implements (ported from
the same reference, `0366`), confirming `IOWeight` is the correct
systemd unit property name and the conversion is identical for both
drivers.

## Implementation

- `oci_runtime_core::systemd_cgroup::resource_properties` gained a
  new arm: `if let Some(block_io) = &resources.block_io && let
  Some(weight) = block_io.weight && weight != 0 { properties.push(
  ("IOWeight", Value::from(convert_blkio_weight_to_io_weight(weight
  as u64)))) }` — reusing `convert_blkio_weight_to_io_weight`
  verbatim (newly imported into this module's existing `use crate::
  cgroups::{...}` list; already `pub(crate)`, no visibility change
  needed). A zero weight is skipped entirely, matching `plan_blkio`'s
  own identical rule for the raw-cgroupfs driver. Two new unit tests.
- `resources_from_cli` gained a new trailing `blkio_weight:
  Option<u16>` parameter, added to the "was anything given at all"
  check, and a new `block_io = blkio_weight.map(|weight| LinuxBlockIo
  { weight: Some(weight) })` construction, added to the returned
  `LinuxResources`. Two new unit tests
  (`resources_from_cli_is_some_when_only_blkio_weight_is_given`,
  `resources_from_cli_carries_blkio_weight_into_a_real_linux_block_
  io`).
- `RunArgs` (flattened into both `Command::Run`/`Command::Create`)
  and `Command::Update` (a separate, non-flattened struct) each gain
  `blkio_weight: Option<u16>` (`--blkio-weight`), inserted right
  after `cpu_shares`.
- `synthesize_spec` (shared by `run`/`create`) and `cmd_update` both
  gained the new parameter, threaded straight through to their own
  already-existing `resources_from_cli` calls.
- `cmd_build`'s own call passes `None` — real `podman build` has no
  `--blkio-weight` of its own at all (checked directly, absent from
  buildah's own `CommonBuildOptions`), a genuinely `run`/`create`/
  `update`-only flag, unlike every resource flag `0453`-`0462`
  already ported to `build` too.
- `cmd_update`'s own "no resource or health flags given" error message
  updated to mention `--cpu-period`/`--cpu-quota`/`--cpu-shares`/
  `--blkio-weight` too (a pre-existing omission from `0462`, fixed
  here alongside the new flag it was already missing).

## Tests

One new integration test in `tests/tests/ociman_run.rs`
(`run_blkio_weight_sets_the_real_systemd_scopes_own_io_weight` — the
same real, live `systemctl --user show` verification technique
`--cpu-shares`'s own test already established, confirming `IOWeight`
reports back the real, converted value (`500` → `4950`) even though
this dev host's own rootless `systemd --user` session doesn't
delegate the `io` controller at all: the same "property accepted and
correctly reported, real enforcement not guaranteed on every host"
caveat `--cpuset-cpus`/`--cpuset-mems`'s own tests already document
for `AllowedCPUs`). All 109 prior tests in the file pass unmodified
(110/110 total).

**Deliberately no live `ociman update --blkio-weight` integration
test**, matching `0366`'s own already-established precedent for
`ocirun update --blkio-weight` (which has none either, only unit
tests): `ociman update`'s own raw-cgroupfs driver hits the exact same
real, checked-directly host limitation `0366` already documented in
detail (`io.bfq.weight`/`io.weight` genuinely don't exist on this
dev host's own rootless cgroup delegation at all — a real `EACCES`,
not the tolerated `ENOENT`, so the write would be a real, immediate
failure here, correctly matching what real `crun`/`runc` would also
do given the identical kernel condition, not a divergence). A test
that always expects a hard failure on *this* host but should succeed
on a host with proper `io`-controller delegation would be unstable
across environments; the unit tests covering `resources_from_cli`'s
own threading, plus the systemd-path integration test above (a
different code path, unaffected by this specific limitation), already
provide real, meaningful coverage.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (120 test-result
blocks, 0 failures, clean on the first full run), `python3 ci/
guards.py`, `cargo deny check`, `bash ci/native-ci.sh` (clean, 120/120,
clean on the first run too), `bash ci/build-deb.sh` (real `dpkg -i`/
`--version`/`dpkg -r` round trip). This touches `run`/`create`/
`update`'s own real hot path — ran the full `ci/bench.sh` suite to
confirm no regression: all 9 categories show speedups consistent with
previously-recorded baselines.

## Deliberately still out of scope

`--blkio-weight-device`/`--device-read-bps`/`--device-write-bps`/
`--device-read-iops`/`--device-write-iops` (real docker's/podman's own
per-device block-IO controls) remain genuinely out of scope: this
project's own `LinuxBlockIo` models only the plain, whole-cgroup
`weight` field, matching `ocirun update --blkio-weight`'s own already-
established, deliberately narrow scope (`0366`) — per-device weights/
rate limits would need a real new `LinuxWeightDevice`/`LinuxThrottle
Device` type modeled from scratch, a larger, separately-shaped
feature. `--cpu-rt-period`/`--cpu-rt-runtime`/`--memory-swappiness`
remain the other genuinely out-of-scope `ociman update` flags (no
cgroup v2 equivalent exists for either).
