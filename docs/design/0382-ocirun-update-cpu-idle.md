# Design note 0382: `ocirun update --cpu-idle`

Status: implemented
Scope: `crates/oci-spec-types/src/runtime.rs`,
`crates/oci-runtime-core/src/cgroups.rs`,
`crates/oci-runtime-core/src/systemd_cgroup.rs`,
`bin/ocirun/src/main.rs`, `tests/tests/ocirun_update.rs`.

## What this closes

`ocirun update` had every ad-hoc CPU-bandwidth flag real `runc update`
supports except `--cpu-idle` (`~/git/runc/update.go`: "set cgroup
SCHED_IDLE or not, 0: default behavior, 1: SCHED_IDLE") — the last
item `0356`'s own doc comment explicitly named as still deferred.
`oci_spec_types::runtime::LinuxCpu` had no `idle` field at all, so a
`--resources file.json` containing `{"cpu":{"idle":1}}` was silently
dropped on parse — a real, un-flagged data-loss bug in the JSON-
passthrough path both real runc and crun honor, arguably worse than
the missing ad-hoc flag alone.

## Real, checked-directly confirmation

- `Idle *int64` is the real, current upstream `runtime-spec` field
  (`~/git/runc/vendor/github.com/opencontainers/runtime-spec/
  specs-go/config.go`) — only the CLI flag is runc-only; crun has no
  `--cpu-idle` flag but does honor the field via its own `update
  --resources` JSON path (`~/git/crun/src/libcrun/
  cgroup-resources.c:1128-1136`, `cpu->idle_present`).
- **Real cgroup v2 `cpu.idle` file format**, confirmed three
  independent ways (kernel doc `Documentation/admin-guide/
  cgroup-v2.rst`, kernel source `kernel/sched/fair.c`'s own
  `sched_group_set_idle` enforcing exactly `if (idle < 0 || idle > 1)
  return -EINVAL`, and both reference runtimes' own write code): a
  plain decimal `0`/`1`, no `"max"`/sentinel conversion.
- **A real, previously-undiscovered kernel-level ordering hazard**,
  confirmed directly from `kernel/sched/fair.c`: `sched_group_set_
  shares` (backs `cpu.weight`) refuses any write at all once
  `cpu.idle=1` is already in effect (`if (tg_is_idle(tg)) ret =
  -EINVAL`), while `sched_group_set_idle` has no such restriction and
  silently resets the group's own effective weight as a side effect
  regardless of what was just written. This is exactly why real
  runc's own fs2 driver (which writes `cpu.idle` *before*
  `cpu.weight`) has to defensively force `Shares = 0` whenever
  `--cpu-idle` is given (`update.go:267-270`) — real crun's raw
  cgroupfs driver writes weight *then* idle and needs no such
  workaround, since a later `cpu.idle` write always wins with no
  error either way. **Verified by hand against this host's own real
  kernel**, not just read from source: a combined `--cpu-share 512
  --cpu-idle 1` update genuinely succeeds and `cpu.weight` reads back
  as the kernel's own internal `SCHED_IDLE` value (not 59, what
  `--cpu-share 512` alone converts to), confirming `cpu.idle` truly
  took final effect.
- **No explicit conflict check is needed** at the raw-cgroupfs level:
  matching real crun's own write order (weight, then idle) means the
  kernel itself resolves a same-call `shares`+`idle=1` combination
  silently, with no error — the same outcome real crun's own shipped
  behavior already relies on.
- **The systemd D-Bus properties path is architecturally distinct and
  currently unreachable for this flag**: traced the actual call
  graph and confirmed neither `ocirun update`/`ociman update` ever
  calls into `systemd_cgroup.rs` at all — both go straight through
  `cgroups::plan_resources`/`apply` against the already-`Delegate=
  true`'d cgroup a systemd-driven `ociman run`/`create` already
  created. `resource_properties`'s own translation only matters for
  `create_scope` (initial launch), which today has no real call site
  that can populate `cpu.idle` at all (`ociman run` never had, and
  still doesn't have, a `--cpu-idle`-equivalent flag of its own).
  Added for the shared type's own internal consistency regardless,
  following runc's own real, current systemd v2 driver's tolerant
  precedent (`CPUWeight = 0` tells systemd to configure `cpu.idle`;
  a same-call `shares` is silently skipped, not a hard error like
  real crun's own systemd translation) — chosen because it matches
  this codebase's own already-established "tolerate a conflicting or
  malformed resource property rather than fail the whole launch"
  stance elsewhere in the same function.
- `ociman update`'s own doc comment already establishes a deliberately
  narrower scope matching `ociman run`'s own flag set, which has never
  had any CPU-bandwidth ad-hoc flag at all — confirmed `--cpu-idle`
  is consistently, correctly out of scope there too; no `ociman`
  changes needed.

## A second, unrelated stale-doc-comment bug found and fixed along the
   way

While updating `Command::Update`'s own top doc comment (which had
already, correctly, named `--cpu-idle` as deferred), found it also
still listed `--blkio-weight` as "needing a whole new cgroup v1-to-v2
`io.weight` translation this project doesn't have yet" — but `0366`
had already fully implemented exactly that. The same kind of drift
`docs/design/0380` found and fixed in `ocirun features`'s own hooks
list. Fixed in the same pass.

## Implementation

- `LinuxCpu` gains `pub idle: Option<i64>` (after `mems`).
- `cgroups::plan_cpu` writes `cpu.idle` as a plain decimal, placed
  *after* the existing `cpu.weight`/`cpu.max`/`cpu.max.burst` writes —
  matching real crun's own order, for the real kernel-behavior reasons
  above.
- `systemd_cgroup::resource_properties` translates `cpu.idle ==
  Some(1)` into `CPUWeight = 0`, skipping any shares-derived
  `CPUWeight` in the same call — matching real runc's own tolerant
  systemd v2 driver precedent, documented as currently-unreachable
  dead code from any real call site today, kept purely for the shared
  type's own consistency. No systemd-version gate (real runc's own
  driver requires systemd ≥ 252 for this; both of this project's
  documented first-class targets, CentOS Stream 10 and Ubuntu 26.04,
  ship well above that — a deliberate, stated simplification).
- `ocirun`'s `Command::Update` gains `--cpu-idle` (`allow_hyphen_
  values` — matching `--cpu-quota`/`--cpu-rt-runtime`'s own reason for
  it, since `SCHED_IDLE`'s own real spec type is signed); threaded
  through the dispatch match arm, `UpdateFlags`, and `resources_from_
  flags` exactly like every other existing ad-hoc CPU flag.

## Tests

New unit tests: `crates/oci-runtime-core/src/cgroups.rs`
(`cpu_idle_writes_a_plain_decimal_with_no_max_conversion`,
`cpu_idle_and_shares_together_write_weight_before_idle` — asserting
the real write order); `crates/oci-runtime-core/src/systemd_cgroup.rs`
(three tests covering the `CPUWeight=0` translation, the shares-wins-
skipped case, and confirming `idle: Some(0)` does *not* trigger the
special-value translation); `bin/ocirun/src/main.rs`
(`resources_from_flags_builds_cpu_idle_alone`, and `cpu_idle` folded
into the existing combined-CPU-bandwidth-fields test). New integration
test in `tests/tests/ocirun_update.rs`:
`update_cpu_idle_combined_with_cpu_share_succeeds_and_idle_wins` — a
real, kernel-enforced verification against a real `systemd --user`-
delegated cgroup: a combined `--cpu-share`/`--cpu-idle` update
succeeds cleanly, `cpu.idle` reads back correctly, `cpu.weight`
genuinely reflects the kernel's own internal `SCHED_IDLE` value (not
the value `--cpu-share` alone would have produced) — proving `cpu.idle`
took real, final effect, not just "no error was returned" — and
setting `--cpu-idle 0` afterward restores completely ordinary
`cpu.weight` behavior for a later plain `--cpu-share` update. All 10
tests in the file pass.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures), `python3
ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh`, `bash
ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg -r` round trip).
This change touches `cgroups::plan_cpu`/`systemd_cgroup::resource_
properties`, both on the real container-launch/update hot path — the
new code is a single `if let Some(idle) = ...`/`if cpu.idle ==
Some(1)` no-op branch unless the field is actually set (which no
current `ociman run`/`create` flag populates); targeted `hyperfine`
re-runs confirm no regression: `ocirun run` 3.4ms, `ociman run --rm`
33.0ms, both matching recent baselines within noise.
