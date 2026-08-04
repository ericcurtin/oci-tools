# Design note 0418: `ociman container prune --filter`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_container.rs`,
`README.md`.

## What this closes

`ociman container prune` had no `--filter` support at all — every
`Created`/`Stopped` container was always unconditionally eligible.
Real `podman container prune --filter` narrows this to `label=`/
`label!=`/`annotation=`/`annotation!=`/`until=`. This closes the
`label=`/`label!=`/`until=` slice — the honest, narrower target this
project's own single-labels-concept model actually has (see below).

## Real, checked-directly confirmation

`~/git/podman/pkg/domain/filters/containers.go`'s own
`GeneratePruneContainerFilterFuncs`:

```go
switch filter {
case "label":
    return func(c *libpod.Container) bool {
        return filters.MatchLabelFilters(filterValues, c.Labels())
    }, nil
case "label!":
    ...
case "annotation":
    return func(c *libpod.Container) bool {
        return filters.MatchLabelFilters(filterValues, c.ConfigNoCopy().Spec.Annotations)
    }, nil
case "annotation!":
    ...
case "until":
    return prepareUntilFilterFunc(filterValues)
}
return nil, fmt.Errorf("%s is an invalid filter", filter)
```

Two things confirmed directly, not assumed: real `podman container
prune` genuinely has **no** `dangling=` key at all (that's an
image-only concept in real podman too, not something this project
narrowed away) — the default `switch` falls through to a hard error
for anything else, matching this command's own new, equally strict
rejection. And real podman's own separate `annotation=`/`annotation!=`
keys operate on the real OCI-spec-level `Spec.Annotations`, distinct
from `c.Labels()` — a real, separate concept this project has no
equivalent split for at all (every label this project stores already
lives in one place, `ANNOTATION_LABELS`), so only `label=`/`label!=`
are implemented here, a deliberate, honest narrowing rather than a
silent gap.

## Implementation

- New `ContainerPruneFilters` struct (`labels: Vec<LabelFilter>`,
  `until: Option<SystemTime>`) and `parse_container_prune_filters`,
  placed next to the existing `PruneFilters`/`parse_prune_filters`
  they closely mirror. `labels` are OR'd together, matching this
  project's own already-established `ociman prune --filter label=`
  convention (`PruneFilters::labels`'s own doc comment) rather than
  `ociman ps --filter label=`'s AND'd one — this command is a
  prune-family sibling, not a listing/visibility filter.
- `ContainerCommand::Prune` gains `filter: Vec<String>`
  (`#[arg(long = "filter")]`).
- `prune_eligible_containers` gains a `filters: &ContainerPruneFilters`
  parameter; matching reuses the exact same annotation-decode-then-
  match pattern `ociman ps --filter label=` already established, and
  the exact same `until`-threshold-vs-`parse_rfc3339_utc(&state.
  created)` comparison already used elsewhere. `cmd_prune`'s own
  call site (`ociman prune`'s container-removal phase) passes
  `ContainerPruneFilters::default()`, preserving its existing,
  unfiltered behavior exactly — whether `ociman prune --filter`
  should also reach that phase (matching real podman's own `system
  prune` forwarding the same filters to both images and containers)
  is a natural, separate, deliberately deferred follow-up.

## Tests

Two new tests in `tests/tests/ociman_container.rs`:
`container_prune_filter_label_only_removes_a_matching_stopped_
container` (two `Created` containers, one matching `--filter
label=env=prod`, one not; only the matching one is removed) and
`container_prune_filter_until_keeps_a_freshly_created_container`
(asserts a fresh container survives `--filter until=1h`, then — after
a real `sleep` — is removed by `--filter until=1s`, the same
established technique `ociman_prune.rs`'s own `prune_filter_until_
removes_an_image_older_than_the_threshold` already uses). All 3
prior `container_prune_*` tests continue to pass unmodified (5/5
total).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean, no diff), `cargo clippy --workspace --all-targets --locked --
-D warnings`, `cargo test --workspace --locked` (119 test-result
blocks, 0 failures, clean on the first full run), `python3 ci/
guards.py`, `cargo deny check`, `bash ci/native-ci.sh` (clean,
119/119), `bash ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg
-r` round trip). Touches only `ociman container prune`'s own
eligibility filter, not any hot path at all — no benchmark re-run
needed.

## Deliberately still out of scope

`annotation=`/`annotation!=` (real podman's own separate OCI-spec-
annotations concept, distinct from labels — see above) and forwarding
`ociman prune --filter`'s own values into its container-removal phase
too. Also noted while researching this: real `podman rm`/`stop`/
`restart`/`pause`/`unpause` **all** accept a full `--filter` using the
same rich grammar `ps` already has (`status=`/`id=`/`name=`/
`command=`/`before=`/`since=`/`ancestor=`/`exited=`, on top of
`label=`/`until=`) — a real, systemic, sibling gap across five
commands at once, deliberately not pursued here: a small, honest
version would need the same `label=`/`until=`-only narrowing this
increment establishes, while a fully faithful one first needs `cmd_
ps`'s own inline per-container matching closure extracted into a
shared, reusable function — either is real, separate, future work.
