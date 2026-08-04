# Design note 0413: `ociman volume ls --filter dangling=true|false`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_volume.rs`,
`README.md`.

## What this closes

Closes the single most plausible remaining candidate `0412`'s own
"Deliberately still out of scope" section explicitly flagged, since
`containers_using_volume` already computes the exact real answer
needed (already shared by `rm`/`prune`/`rename`'s own dependency
checks). `ociman volume ls --filter` now has real, working coverage
of every key its own most common real-world uses would reach for.

## Real, checked-directly confirmation

- `~/git/podman/pkg/domain/filters/volumes.go`'s own `case "dangling":`
  branch: validates every value is `true`/`1`/`false`/`0` up front,
  then matches via `v.IsDangling()`.
- `~/git/podman/libpod/volume.go`'s own `IsDangling`: `ctrs, err :=
  v.VolumeInUse(); return len(ctrs) == 0, nil` — a real, checked-
  directly "no container references it at all" check, the identical
  concept this project's own `containers_using_volume` already
  computes (a bundle-mount-based scan, not a live-process check —
  needs no running container at all, matching real podman's own
  storage-layer `VolumeInUse` check rather than anything runtime-
  state-dependent).

## A deliberate divergence from real podman's own raw per-value loop

Real podman's own `dangling=` matcher iterates every given filter
value and returns true on the first match — giving both `true` and
`false` together is therefore a real, silent no-op that matches every
volume regardless (each volume is either dangling or not; OR-ing both
outcomes together is tautologically true). This project instead
reuses the exact same shared `try_parse_dangling_filter`/"conflicting
dangling filter values specified" convention `ociman prune`/`images
--filter dangling=` already established — a deliberate divergence,
not an oversight: this project's own established rule already treats
giving contradictory values for this exact key as a clear, immediate
error everywhere else it appears, and there is no good reason for
`volume ls` alone to silently accept the one input shape that rule
exists to catch.

## Implementation

- `VolumeCommand::Ls`'s own `filter` field doc comment gains
  `dangling=true|false`, including the deliberate-divergence
  reasoning above.
- `cmd_volume_ls` reuses `try_parse_dangling_filter` (already shared
  by `prune`/`images`) for parsing, then `Vec::retain`s by calling
  the already-existing `containers_using_volume` once per remaining
  volume record — the same "no precomputed set, one call per volume"
  shape `cmd_volume_prune` itself already uses for the identical real
  question, not a new performance concern this change introduces.

## Tests

One new end-to-end integration test in `tests/tests/ociman_volume.rs`,
`volume_ls_filter_dangling_selects_only_unreferenced_or_only_
referenced_volumes` — a real volume no container ever references and
a second, real volume a genuinely persisted (`create`d, never
started — needs no live process at all, matching `IsDangling`'s own
storage-layer-only check) container references via `-v name:/data`;
`dangling=true`/`dangling=1` correctly match only the unused one,
`dangling=false`/`dangling=0` correctly match only the in-use one, and
giving both together is a clear, immediate "conflicting dangling
filter values" error. All existing tests continue to pass unmodified
(38/38 in `ociman_volume.rs`).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures, clean on
the first full run), `python3 ci/guards.py`, `cargo deny check`,
`bash ci/native-ci.sh`, `bash ci/build-deb.sh` (real `dpkg -i`/
`--version`/`dpkg -r` round trip). This touches only `ociman volume
ls`'s own filter-matching path, not any hot path at all — no
benchmark re-run needed.

## Deliberately still out of scope

`driver=`/`label=`/`label!=`/`opt=` remain unimplemented — each needs
real schema/data (per-volume labels, driver options) this project's
fixed "local directory" volume model doesn't store at all yet, a
real, separate, bigger gap than any of the filter keys closed across
`0410`-`0413`.
