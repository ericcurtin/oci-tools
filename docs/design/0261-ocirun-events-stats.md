# Design note 0261: `ocirun events --stats`

Status: implemented
Scope: `bin/ocirun/src/main.rs`, `tests/tests/ocirun_events.rs`.

## Closing a real, checked-directly `ocirun`-vs-`runc` gap

Comparing `ocirun --help`'s own subcommand list against a real
installed `runc --help` directly turned up exactly three commands
`ocirun` didn't have: `checkpoint`/`restore` (need real CRIU
integration — a large, external-dependency project, deliberately left
alone) and `events`. Unlike checkpoint/restore, `events --stats` is a
narrow, low-risk, well-scoped gap: a one-shot JSON dump of a running
container's real cgroup stats, used by higher-level tooling
(containerd, monitoring scripts) that expect exactly this subcommand
from a `runc`-compatible runtime.

## Pure composition, narrower but honest

Every primitive this needed already existed and was already
tested — `oci_runtime_core::cgroups::{cpu_usage_nanos, memory_current_
bytes, memory_limit_bytes, pids_current}` and `resolve_cgroup_dir`
(shared with `ocirun update`/`pause`/`resume`). The only new code is
the JSON envelope and one new subcommand's worth of glue.

Real runc's own `types.Stats` (`~/git/runc/types/events.go`) is much
larger — cpuset, blkio, hugetlb, Intel RDT, PSI, per-interface network
counters — none of which this project has a reader for anywhere. This
slice reports a deliberately narrower subset, matching this project's
own established "honest, smaller-but-real report, never a fabricated
one" convention (e.g. `ociman info`) rather than attempting a
byte-for-byte port. Every field it *does* report was checked directly,
field for field, against real runc's own actual cgroup-v2 collection
code (`~/git/runc/vendor/github.com/opencontainers/cgroups/fs2/
{cpu,memory}.go`), not guessed from the JSON struct's own field names:

- `cpu.usage.total` — `cpu.stat`'s `usage_usec * 1000` (nanoseconds),
  matching real runc's own identical `TotalUsage = v * 1000`.
- `memory.usage.usage` — the *raw* `memory.current` (real runc's own
  `getMemoryDataV2`), deliberately **not** the working-set-adjusted
  value `ociman stats`'s own, differently-purposed display uses (that
  command intentionally reports the docker/podman-style "usable"
  number; `runc events --stats` reports the raw cgroup value).
- `memory.usage.limit` — `memory.max`, with the real kernel's own
  `"max"` sentinel mapped to `u64::MAX` — confirmed directly against
  real runc's own `GetCgroupParamUint`'s identical `math.MaxUint64`
  mapping, field has no `omitempty` on the Go side either, so this
  project's own struct always serializes it too.
- `pids.current` — `pids.current`, unchanged.

The periodic (no `--stats`, every-`--interval`, OOM-notify) mode real
`runc events` also has is a clear, honest "not yet implemented" error
instead of a half-implemented approximation — the same shape `ociman
stats` already established for its own missing streaming mode.

## Verified

Integration (`tests/tests/ocirun_events.rs`), against a real,
delegated `systemd --user` cgroup subtree (same fixture
`ocirun_update.rs`'s own tests already established):

- `events --stats` on a real running container returns one real JSON
  line with the exact `{"type":"stats","id":...,"data":{...}}` shape;
  `cpu.usage.total`/`memory.usage.usage`/`pids.current` are all real,
  nonzero numbers; an unset `memory.max` maps to the real `u64::MAX`
  sentinel, cross-checked directly against the same cgroup file
  `ocirun update`'s own tests already read.
- `events` without `--stats` is the clear, documented "not yet" error.
- `events --stats` of an already-stopped container is a clear error.

Full workspace: `cargo build`/`test --workspace` (108 test binaries),
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets --
-D warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh`.

## Still ahead

`checkpoint`/`restore` (real CRIU integration, a materially larger,
external-dependency project) remain `ocirun`'s own last real gaps
versus `runc`'s own CLI surface. The periodic/OOM-notify `events` mode
(no `--stats`) stays a documented, honest gap.
