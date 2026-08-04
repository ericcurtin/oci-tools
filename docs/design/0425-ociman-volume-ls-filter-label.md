# Design note 0425: `ociman volume ls --filter label=`/`label!=`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_volume.rs`,
`README.md`.

## What this closes

`ociman volume ls --filter` had no `label=`/`label!=` support — a
gap that could only be closed once `ociman volume create --label`
(`0424`) landed the underlying schema (a real, per-volume `labels`
field) to filter on at all. `ociman volume ls --filter` now has
real, working coverage of `name=`/`label=`/`label!=`/`until=`/
`after=`/`since=`/`dangling=` — every key its own most common
real-world uses would reach for.

## Real, checked-directly confirmation — a genuinely useful finding

`~/git/podman/pkg/domain/filters/volumes.go`'s own `case "label":`
uses `filters.MatchLabelFilters` — the exact same function real
podman's own *container* label filtering also uses (confirmed in
`0274`'s own research note, cited by `ociman ps --filter label=`'s
own doc comment). But which combination rule actually applies
depends on **how many values get bundled into one call**, not just
which function is used — so this needed tracing one level higher:
`~/git/podman/pkg/domain/infra/abi/volumes.go`'s own `VolumeList`:

```go
for filter, value := range opts.Filter {
    filterFunc, err := filters.GenerateVolumeFilters(filter, value, ic.Libpod)
    ...
}
```

`opts.Filter` is `map[string][]string` — every `--filter label=`
value given on the command line is grouped under the one shared
`"label"` key and passed to `GenerateVolumeFilters` **in a single
call**, so `MatchLabelFilters` sees every value at once and requires
**all** of them to match (its own internal `for _, filterValue :=
range filterValues { ... if no label matches, return false }` loop).
This is genuinely different from `images`/`prune --filter label=`'s
own OR combination (`~/git/container-libs/common/libimage/
filters.go`'s own `filterLabel`, compiled and combined *per value*,
then OR'd by the outer `applyFilters` loop) — confirmed by tracing
the actual call site, not assumed from the shared function name
alone. `ociman volume ls --filter label=` therefore matches `ociman
ps --filter label=`'s own already-established AND convention, not
`images`'/`prune`'s own OR one — a real, deliberate, checked-directly
divergence from this project's own `images`/`prune` sibling filters,
spelled out explicitly in the new doc comment so a future reader
doesn't assume the wrong one by analogy.

## Implementation

- `cmd_volume_ls` reuses the exact same, already-proven
  `try_parse_label_filter`/`LabelFilter` primitives every other
  `label=`/`label!=` filter in this codebase already shares —
  parsing is identical to every sibling; only the *combination rule*
  (`.all()` instead of `.any()`) differs, matching `ps`'s own
  `PsFilters::labels` shape exactly rather than `PruneFilters`'/
  `ImageFilters`' `.any()` shape.
- `VolumeCommand::Ls::filter`'s own doc comment spells out the real
  divergence and its exact confirmation, matching this project's own
  established practice of documenting every checked-directly
  divergence inline, not just in the design note.

## Tests

One new test in `tests/tests/ociman_volume.rs`,
`volume_ls_filter_label_multiple_values_are_anded_together`: two
labeled volumes, asserting a single `label=` value matches correctly,
two values under the same key together (AND) narrow to only the
volume matching both, a third, unsatisfiable combination matches
nothing, and `label!=` correctly excludes a match. All 43 prior
tests in `ociman_volume.rs` continue to pass unmodified (44/44
total).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean, no diff), `cargo clippy --workspace --all-targets --locked --
-D warnings`, `cargo test --workspace --locked` (119 test-result
blocks, 0 failures, clean on the first full run), `python3 ci/
guards.py`, `cargo deny check`, `bash ci/native-ci.sh` (clean,
119/119), `bash ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg
-r` round trip). Touches only `ociman volume ls`'s own filter-
matching path, not any hot path at all — no benchmark re-run needed.

## Deliberately still out of scope

`driver=`/`opt=` remain unimplemented — each needs real schema/data
(driver options) this project's fixed "local directory" volume model
doesn't store at all, and would be a pure no-op flag even if parsed
(the same reasoning `0423`/`0424`'s own design notes already gave for
`volume create --driver`/`--opt`).
