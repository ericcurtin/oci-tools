# Design note 0342: `ociman ps -s`/`--size`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_ps.rs`.

## What this closes

`-s`/`--size` (each listed container's own writable-layer size and
total rootfs size) was already flagged as a candidate in `0290`'s own
"still ahead" list, alongside `ociman ps`'s other not-yet-scoped
flags. This closes it.

## Real, checked-directly semantics

Read real podman's own source directly (`~/git/podman/cmd/podman/
containers/ps.go`'s own `psReporter.Size()`, `~/git/podman/libpod/
container.go`'s own `RootFsSize`/`RWSize`, and `~/git/podman/libpod/
container_internal.go`'s own lowercase `rootFsSize`/`rwSize`):

- `--size`/`-s` is opt-in and computed **only** when given — real
  podman's own `ListContainer.Size` field stays `nil` (and is omitted
  from JSON entirely) unless `opts.Size` was set, since it needs a
  real directory walk plus a storage-layer size lookup per container,
  work an ordinary `ps` shouldn't pay for.
- The displayed value is always a pair: `<rw size> (virtual <root fs
  size>)`. `RwSize` is the container's own writable-layer size alone.
  `RootFsSize` is genuinely `imageSize + layerSize` — the *same*
  `layerSize` number `RwSize` itself reports, not two independently
  computed figures — so `RootFsSize` is always `>= RwSize`.
- Real podman's own formatting (`units.HumanSizeWithPrecision(x, 3)`)
  uses **3** significant digits, genuinely coarser than this project's
  own existing `human_size` helper's `4`-digit default (already used
  by `ociman stats`/`system df`, itself approximating real go-units'
  own separate `HumanSize` = `HumanSizeWithPrecision(x, 4)`) — a real,
  checked-directly difference between two different real call sites in
  the same upstream codebase, not an inconsistency to "fix" one way or
  the other.
- `--size`/`-s` conflicts with `--quiet` (real podman's own
  `checkFlags`: `"quiet conflicts with size and namespace"`) — this
  project has no `--namespace` flag to conflict with, so the ported
  error only ever names `--size`.

## Implementation

`container_writable_layer_size(bundle_dir: &Path) -> u64` is a new,
shared helper factored out of `cmd_system_df`'s own two, previously-
duplicated inline copies of the exact same "check `upper/`'s own
presence first, else fall back to `rootfs/` itself" computation
(`docs/design/0108`-`0110`'s own rootless-overlay optimization) — a
pure, verified-unchanged refactor (all 11 of `system df`'s own
existing tests, including its own container-size-dependent ones, pass
completely unmodified) done the moment a third caller (`ps --size`)
needed the identical thing.

New `compute_container_size(store, state) -> ContainerSizeView`
mirrors real podman's own exact formula: `rw_size` is
`container_writable_layer_size`'s own result; `root_fs_size` adds the
container's own resolved image's total stored size (`Store::
image_summary`, the same one `ociman images`/`system df` already use)
on top, or `0` if the image reference is missing or no longer resolves
(an already-`rmi`'d image) — infallible, matching real podman's own
"log and continue" behavior for this same best-effort display feature
rather than failing the whole `ps` call.

`ContainerView` gained a new `size: Option<ContainerSizeView>` field
(`#[serde(skip_serializing_if = "Option::is_none")]`, matching
`name`'s own established convention) — `None` unless `--size` was
given, so the image store is only ever opened
(`size.then(open_store).transpose()?`) and the per-container
computation only ever runs when actually requested, matching real
podman's own identical on-demand-only cost model.

`human_size` (existing, 4-digit-precision) is now a thin wrapper
around a new, more general `human_size_with_precision(bytes,
precision)` — needed since real go-units' own `HumanSizeWithPrecision`
genuinely is called with different precisions by different real
callers (`HumanSize` itself: `4`; `ps.go`'s own `Size()`: `3`), not a
single constant this project could hardcode once. Every existing
caller of `human_size` is unaffected (its own behavior is exactly
`human_size_with_precision(_, 4)`, unchanged).

`--format` needs no code changes at all to reach `{{.size.rw_size}}`/
`{{.size.root_fs_size}}`: the existing `render_format_template`/
`resolve_json_path` engine already walks any nested JSON shape
generically via `serde_json::to_value`, so this "just works" once
`ContainerSizeView` exists and is populated — verified directly with a
new test.

## Verified

New integration tests in `ociman_ps.rs`:
`ps_size_flag_shows_a_real_size_and_virtual_total` (plain `ps` shows
no size info at all; `--size` shows a `SIZE` header and a `(virtual
...)` figure), `ps_size_short_flag_behaves_identically_to_the_long_form`,
`ps_quiet_and_size_together_is_a_clear_error`,
`ps_json_only_includes_size_when_the_size_flag_is_given` (including
that `root_fs_size >= rw_size` always holds), and
`ps_format_can_reach_the_nested_size_fields_when_size_flag_given`
(both the negative case — unresolvable without `--size` — and the
positive one).

New unit tests: `human_size_with_precision_3_matches_real_podman_ps_
sizes_own_precision`, `human_size_with_precision_4_matches_human_size_
exactly`.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (113 test-result blocks,
0 failures), `python3 ci/guards.py`, `cargo deny check`.

## Still ahead

`ociman ps --sort` (real podman's own `command`/`created`/`id`/
`image`/`names`/`runningfor`/`size`/`status` sort keys) and
`--namespace` (real cgroup/ipc/mnt/net/pidns/user/uts per-container
namespace inode listing) remain separate, not-yet-scoped `ps` gaps.
`ocibox create/enter --hostname` (an override on top of `0292`'s
already-correct default) remains an open, similarly-small candidate
surveyed alongside this one.
