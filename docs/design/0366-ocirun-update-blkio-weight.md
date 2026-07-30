# Design note 0366: `ocirun update --blkio-weight`

Status: implemented
Scope: `crates/oci-spec-types/src/runtime.rs`,
`crates/oci-runtime-core/src/cgroups.rs`, `bin/ocirun/src/main.rs`.

## What this closes

The last remaining ad-hoc `ocirun update` flag flagged (but
deliberately deferred, as needing new plumbing) in `0356`'s own "still
ahead" section: `--blkio-weight`, present on both real `runc update`
and `crun update`.

## Real, checked-directly semantics

Read both reference runtimes' own cgroup v2 drivers directly:

- `~/git/runc/vendor/.../cgroups/fs2/io.go`'s own `setIo`: if a real
  `io.bfq.weight` file exists (opened successfully), write the real
  spec's own raw value there directly (`blkio.weight`'s documented
  `[10-1000]` range, no conversion — `io.bfq.weight` uses the same
  range). Otherwise, fall back to `io.weight` with a linear
  conversion to its own `[1-10000]` range
  (`cgroups.ConvertBlkIOToIOWeightValue`: `y = 1 + (x - 10) * 9999 /
  990`). Runc's own version also does an extra read-back heuristic to
  detect *per-device* BFQ weight support — irrelevant here, since this
  slice only ever models the plain `weight` field, not per-device
  weights.
- `~/git/crun/src/libcrun/cgroup-resources.c`'s own
  `write_blkio_resources`: simpler — just *try* writing
  `io.bfq.weight` directly; on a real `ENOENT` specifically, fall back
  to `io.weight` with the identical conversion formula. This project
  follows crun's own simpler approach for the identical real effect
  (no read-back heuristic needed for the plain, non-per-device case).
- Neither reference tool validates the CLI value's own range at all
  (`runc update.go`: a bare `uint16(cmd.Int(...))` cast) — matched
  here too, no CLI-side validation.

## Implementation

New `LinuxBlockIo { weight: Option<u16> }` in `oci-spec-types`,
plus `LinuxResources::block_io` (real spec field name `blockIO`,
explicit `#[serde(rename = "blockIO")]` since the struct has no
blanket `camelCase` rule and serde's own automatic conversion
wouldn't produce the real spec's non-standard capital-I/O form
anyway).

`oci_runtime_core::cgroups::plan_resources` gained `plan_blkio`: always
plans an `("io.bfq.weight", <raw value>)` write — deliberately *never*
the converted value at plan time, since whether BFQ is actually active
can only be known by trying against a real cgroup directory.
`apply` gained the actual two-step logic: on a real `io::ErrorKind::
NotFound` writing `io.bfq.weight`, parse the raw value back out of the
already-planned write, convert it (new `convert_blkio_weight_to_io_weight`,
the same documented formula), and write `io.weight` instead.

New CLI flag `ocirun update --blkio-weight <VALUE>` (`u16`), threaded
through the existing `UpdateFlags`/`resources_from_flags` (`0353`/
`0356`'s own established shape) with no new dispatch complexity.

## A real, checked-directly host-environment finding (not a bug)

Manually verified end to end against this dev host's own real,
delegated `systemd --user` cgroup subtree: neither `io.bfq.weight` nor
`io.weight` exist there at all (`cgroup.subtree_control` only
delegates `cpu memory pids`, confirmed directly, `io.pressure` — an
always-visible stats file, unrelated to actual resource control — is
the only `io.*` file present). The real, observed failure is
`EACCES` ("Permission denied"), not `ENOENT` — and real crun's own
identical code, read directly, *also* only special-cases `ENOENT`;
any other error (including this exact `EACCES`) propagates as a real,
immediate failure there too. This project's own behavior on this host
therefore already matches what real crun would do given the identical
underlying kernel condition — a real host/environment limitation
(the same `io`-controller-not-delegated fact that also makes a live,
in-test cgroup-write assertion impractical here), not a divergence
from either reference tool.

## Verified

New unit tests in `crates/oci-runtime-core/src/cgroups.rs`:
`plan_blkio_carries_the_raw_v1_range_weight_unconverted`;
`plan_blkio_skips_a_zero_weight`;
`convert_blkio_weight_to_io_weight_matches_the_real_documented_formula`
(the documented range endpoints, `10`->`1` and `1000`->`10000`, plus
`500`->`4950`); `apply_writes_the_raw_value_to_io_bfq_weight_when_it_
succeeds`; `apply_falls_back_to_io_weight_with_the_converted_value_
when_io_bfq_weight_is_absent` (a broken symlink whose own target
directory doesn't exist either — a real, genuine `ENOENT` on open,
the same real error a cgroup lacking the BFQ scheduler gives for the
identical file, simulated without needing a real cgroupfs mount at
all). `resources_from_flags_builds_blkio_weight_alone` in
`bin/ocirun/src/main.rs`. Live, real-cgroup end-to-end verification
attempted manually (see above) but not encoded as an automated test,
for the same reason `--cpuset-cpus`/`--cpuset-mems` (`0353`) aren't
either: this dev host's own delegated subtree doesn't enable the
relevant controller (`cpuset`/`io` respectively) at all.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures, full clean
run, no flakes), `python3 ci/guards.py`, `cargo deny check`, `bash
ci/native-ci.sh`, `bash ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip). Not on any `ci/bench.sh`-measured path
(`ocirun update` isn't one of the tracked comparisons), so no
benchmark re-verification needed for this specific change.
