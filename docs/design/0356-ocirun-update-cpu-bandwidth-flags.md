# Design note 0356: `ocirun update`'s ad-hoc CPU-bandwidth flags

Status: implemented
Scope: `bin/ocirun/src/main.rs`, `tests/tests/ocirun_update.rs`.

## What this closes

`0353`'s own "still ahead" list named real runc's remaining ad-hoc
`update` flags. Of those, `--cpu-period`/`--cpu-quota`/`--cpu-share`/
`--cpu-burst`/`--cpu-rt-period`/`--cpu-rt-runtime` map *directly* onto
`oci_spec_types::runtime::LinuxCpu` fields this project's own
`oci_runtime_core::cgroups::plan_cpu` already knows how to translate
(or, for the last two, already knows to correctly leave untranslated)
— no new struct fields, no new cgroup-write logic, the exact same
"pure CLI plumbing onto an already-fully-wired primitive" shape
`0353`/`0351` both already established. `--blkio-weight` (needs a
whole new cgroup v1-to-v2 `io.weight` translation this project doesn't
have) and `--cpu-idle` (needs a new `LinuxCpu` field *and* a new
`cpu.idle` cgroup write, neither of which exist yet) are both real,
genuinely bigger gaps, correctly not picked up here.

## Real, checked-directly semantics

Read `~/git/runc/update.go` directly: `--cpu-share`/`--cpu-period`/
`--cpu-rt-period`/`--cpu-burst` parse as plain `uint64` strings (no
unit suffix at all, unlike `--memory`); `--cpu-quota`/`--cpu-rt-
runtime` parse as plain `int64` strings. Checked `~/git/crun/src/
update.c` too: crun supports the identical five flags **except**
`--cpu-burst`, a real runc-only addition (`~/git/crun/src/update.c`'s
own `options[]` table has no entry for it at all) — the one
genuine, checked-directly asymmetry between the two reference
runtimes for this particular flag set.

`oci_runtime_core::cgroups::plan_cpu` (unchanged by this note, already
fully correct): `shares` → `cpu.weight` (via the already-tested
`convert_cpu_shares_to_weight`); `quota`/`period` → the combined
`cpu.max` file; `burst` → `cpu.max.burst`. `realtime_runtime`/
`realtime_period` are deliberately **not** written to any cgroup file
at all — `LinuxCpu::realtime_period`'s own existing doc comment
already explains why: cgroup v2 has no realtime-scheduling controller.
Verified directly rather than assumed this turn (not just trusting the
doc comment): a real update with `--cpu-rt-period`/`--cpu-rt-runtime`
given leaves `cpu.max` completely byte-for-byte unchanged.

## Implementation

Six new `Command::Update` fields. `Command::Update`'s own parameter
count reached eleven once these were added (five from `0353` plus
`resources` plus these six), so `cmd_update`/`resources_from_flags`'
own growing positional-argument lists were bundled into one new
`UpdateFlags<'a>` struct (`#[derive(Default)]`, every field an
`Option`) — a pure ergonomics change at the call site, not a behavior
one: every field is still exactly the one CLI flag it always was,
just passed as one struct instead of eleven positional arguments.
`resources_from_flags` now populates `LinuxCpu`'s six bandwidth fields
directly from the matching `UpdateFlags` fields whenever *any* of
`cpuset_cpus`/`cpuset_mems`/the six new ones is given (the same
"build the `LinuxCpu` struct once, lazily" pattern `0353` already
established for `cpuset_cpus`/`cpuset_mems` alone).

## Verified

New unit test: `resources_from_flags_builds_every_cpu_bandwidth_
field_together` (all six fields set at once, checked individually).

New integration tests in `ocirun_update.rs`:
`update_ad_hoc_cpu_bandwidth_flags_write_the_real_cgroup`
(`--cpu-share 1024` — real docker/podman/runc's own default CPU
shares value — converts to the already-tested, recognizable
`cpu.weight=100`; `--cpu-period`/`--cpu-quota` combine into the real
`cpu.max` file; `--cpu-burst` writes `cpu.max.burst`, all against a
real, running container's own real delegated cgroup subtree);
`update_cpu_rt_flags_are_accepted_but_write_nothing_to_any_real_
cgroup_file` (a real, direct proof of the "accepted on parse, never
acted on" claim above, not just trusting the existing doc comment:
`cpu.max`'s own content, snapshotted before and after, is asserted
byte-for-byte identical).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (113 test-result blocks,
0 failures — one transient, unrelated `ocicri_container` flake under
full parallel load, reproduced as passing both in isolation and on a
clean full re-run), `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg -i`/`--version`/`-r`
round trip).

## Still ahead

`--blkio-weight` (needs a new cgroup v1-to-v2 `io.weight`
translation), `--cpu-idle` (needs a new `LinuxCpu` field and a new
`cpu.idle` cgroup write), real runc's own explicitly `Hidden`/
"obsoleted; do not use" `--kernel-memory`/`--kernel-memory-tcp`, and
real runc's own Intel RDT-only `--l3-cache-schema`/`--mem-bw-schema`
remain separate, genuinely bigger, not-yet-scoped candidates — each
needing real new underlying plumbing this note's own flags didn't.
