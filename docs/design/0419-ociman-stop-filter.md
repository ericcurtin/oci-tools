# Design note 0419: `ociman stop --filter`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_stop.rs`,
`README.md`.

## What this closes

`ociman stop` had no `--filter` support at all — real `podman stop
--filter` reuses `ps`'s own full filter grammar (`status=`/`id=`/
`name=`/`command=`/`label=`/`before=`/`since=`/`ancestor=`/`exited=`).
This closes the `label=`/`until=` slice — the same deliberate,
narrower-first-slice scope `ociman container prune --filter` (0418)
already established, reusing its exact grammar and both of its shared
helpers.

## Real, checked-directly confirmation

`~/git/podman/cmd/podman/containers/stop.go:73-75`: `flags.
StringArrayVarP(&filters, "filter", "f", ...)`. `~/git/podman/pkg/
domain/infra/abi/containers.go`'s own `getContainers` (used by
`containerStopImpl`, which every `stop` call goes through):

```go
switch {
case len(options.filters) > 0:
    filterFuncs := ... dfilters.GenerateContainerFilterFuncs(k, v, runtime)
    ctrs, err := runtime.GetContainers(false, filterFuncs...)
    libpodContainers = ctrs
    if len(options.names) > 0 {
        // further narrows the filtered set to the given names
    }
case options.all:
    ...
```

Confirmed directly: when `--filter` is given, it becomes the base
selection (replacing `--all`/explicit names as the primary set),
optionally narrowed further by explicit names if *also* given — a
real, non-trivial interaction this increment deliberately doesn't
attempt yet (see below).

## Implementation

- Generalized the `label=`/`until=`-only filter grammar 0418
  introduced: `ContainerPruneFilters`/`parse_container_prune_filters`
  renamed to `LabelUntilFilters`/`parse_label_until_filters` (now
  taking a `command: &str` for its own error messages, the same
  convention `parse_until_filter_value` already established), and
  the label/until matching logic `prune_eligible_containers` had
  inline is now its own shared `matches_label_until_filters(state,
  filters)` — so `ociman container prune --filter` and `ociman stop
  --filter`'s `label=`/`until=` semantics can never silently drift
  apart from each other.
- `Command::Stop` gains `filter: Vec<String>` (`#[arg(long =
  "filter")]`), documented as its own, separate selection mode:
  mutually exclusive with an explicit `ID`/`--name`, `--cidfile`, and
  `--all` — a real, deliberate narrowing versus real podman's own
  "can combine with explicit names to narrow further" behavior (the
  simplest correct scope for a first slice, avoiding the two-source-
  of-truth reconciliation that combining would need).
- `cmd_stop` gains a `filter: &[String]` parameter; when non-empty,
  a new branch mirrors the existing `--all` loop exactly (an
  already-`Stopped` match silently tolerated, every other failure
  surfaced while every other match is still attempted) but narrows
  to only the containers `matches_label_until_filters` actually
  matches, instead of every container.

## Tests

Two new tests in `tests/tests/ociman_stop.rs`:
`stop_filter_label_only_stops_a_matching_container` (two `Created`
containers, one matching `--filter label=env=prod`, one not; only
the matching one's id is selected/printed) and `stop_filter_
combined_with_all_or_an_explicit_id_is_a_clear_error` (both mutual-
exclusivity cases). All 20 prior `ociman_stop.rs` tests continue to
pass unmodified (22/22 total). `ociman container prune`'s own 5
tests (0418) also continue to pass unmodified after the shared-code
rename/extraction.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean, no diff), `cargo clippy --workspace --all-targets --locked --
-D warnings`, `cargo test --workspace --locked` (119 test-result
blocks, 0 failures, clean on the first full run), `python3 ci/
guards.py`, `cargo deny check`, `bash ci/native-ci.sh` (clean,
119/119), `bash ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg
-r` round trip). Touches only `ociman stop`'s own selection logic,
not any hot path at all — no benchmark re-run needed.

## Deliberately still out of scope

Combining `--filter` with explicit names/`--cidfile`/`--all` to
narrow further (real podman's own richer behavior, confirmed above).
The wider `ps`-grammar keys (`status=`/`id=`/`name=`/`command=`/
`before=`/`since=`/`ancestor=`/`exited=`) — reaching those faithfully
needs `cmd_ps`'s own inline per-container matching closure
(`main.rs`'s own `cmd_ps`) extracted into a shared, reusable function
first, the same blocker `0418`'s own design note already flagged.
`ociman rm`/`restart`/`pause`/`unpause --filter` — the same real,
systemic sibling gap across four more commands, each a natural,
separate future increment reusing this same `LabelUntilFilters`/
`matches_label_until_filters` pair (`restart`/`pause`/`unpause` in
particular share `stop`'s/`kill`'s own `--all`-loop shape closely
enough that each should be a similarly small, mechanical port).
