# Design note 0433: `ociman volume prune --filter`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_volume.rs`,
`README.md`.

## What this closes

`ociman volume prune` had no `--filter` support at all — every
unreferenced volume was always unconditionally reclaimed. This
closes the `label=`/`label!=`/`until=`/`after=`/`since=` slice —
real podman's own exact, deliberately *narrower* filter set for
`volume prune` specifically (distinct from `volume ls`'s own wider
one), confirmed directly rather than assumed from the sibling
command's own grammar.

## Real, checked-directly confirmation

`~/git/podman/cmd/podman/volumes/prune.go:44-46`: `flags.
StringArrayVar(&filter, "filter", []string{}, "Provide filter values
(e.g. 'label=<key>=<value>')")`. `~/git/podman/pkg/domain/filters/
volumes.go`'s own `GeneratePruneVolumeFilters` — a real, **separate**
dispatcher from `GenerateVolumeFilters` (the one `volume ls` uses):

```go
func GeneratePruneVolumeFilters(filter string, filterValues []string, runtime *libpod.Runtime) (libpod.VolumeFilter, error) {
    switch filter {
    case "after", "since":
        return createAfterFilterVolumeFunction(filterValues, runtime)
    case "anonymous":
        return createAnonymousFilterVolumeFunction(filterValues)
    case "label":
        return func(v *libpod.Volume) bool { return filters.MatchLabelFilters(filterValues, v.Labels()) }, nil
    case "label!":
        ...
    case "until":
        return createUntilFilterVolumeFunction(filterValues)
    }
    return nil, fmt.Errorf("%q is an invalid volume filter", filter)
}
```

Confirmed directly: exactly five real keys, genuinely narrower than
`volume ls`'s own (no `name=`/`driver=`/`scope=`/`opt=`/`dangling=`
at all — a real, checked-directly restriction, not this project's
own choice). `anonymous=true|false` is the one key deliberately not
implemented: this project's volume schema has no anonymous-vs-named
distinction anywhere at all (confirmed by grep — every volume here
is always explicitly named), so accepting it would have nothing real
to attach to.

## Implementation

This needed almost no new logic at all — every parsing/matching
primitive (`try_parse_label_filter`, `parse_until_filter_value`,
`earliest_referenced_volume_creation`, `LabelFilter::matches`) was
already fully built and proven by `cmd_volume_ls`'s own `--filter`
implementation (`0410`-`0413`, `0425`); this is almost entirely
wiring plus one loop-guard per filter, reused verbatim rather than
duplicated.

- `VolumeCommand::Prune` changes from a bare unit variant to a struct
  variant with `filter: Vec<String>` (`#[arg(long = "filter")]`).
- `cmd_volume_prune` gains a `filter: &[String]` parameter; parses it
  into `label_filters`/`until_filter`/`after_threshold` using the
  exact same helpers `cmd_volume_ls` already calls, then adds one
  `continue`-guard per filter inside the existing removal loop,
  *before* the existing "is it unreferenced" check — the same
  filter-then-eligibility ordering real podman itself uses.
- Labels are ANDed together (`label_filters.iter().all(...)`),
  matching real podman's own checked-directly `MatchLabelFilters`
  exactly — the identical convention `volume ls --filter label=`
  (`0425`) already established, not `images`/`prune --filter
  label=`'s own OR one (a real, deliberate, already-documented
  divergence between this project's own container-storage-object
  families, not something newly introduced here).

## Tests

Three new tests in `tests/tests/ociman_volume.rs`:
`volume_prune_filter_label_only_removes_a_matching_unreferenced_
volume` (two unreferenced volumes, only the matching one removed),
`volume_prune_filter_until_removes_a_genuinely_older_volume` (the
same real `sleep`-then-`until=1s` technique `ociman_prune.rs`'s own
sibling tests already establish), and `volume_prune_filter_rejects_
ls_only_and_anonymous_keys` (asserting `name=`/`dangling=`/
`anonymous=` are all clear, immediate errors, proving the narrower
key set is actually enforced, not just documented). All 44 prior
tests in `ociman_volume.rs` continue to pass unmodified (47/47
total).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean, no diff), `cargo clippy --workspace --all-targets --locked --
-D warnings`, `cargo test --workspace --locked` (119 test-result
blocks, 0 failures, clean on the first full run), `python3 ci/
guards.py`, `cargo deny check`, `bash ci/native-ci.sh` (clean,
119/119 on retry — one earlier attempt hit the known, pre-existing
`ocicri_container.rs` host-contention flake, confirmed environmental
via an immediate isolated rerun), `bash ci/build-deb.sh` (real
`dpkg -i`/`--version`/`dpkg -r` round trip). Touches only `ociman
volume prune`'s own filter-matching path, not any hot path at all —
no benchmark re-run needed.

## Deliberately still out of scope

`anonymous=true|false` — no target exists in this project's volume
schema at all (see above). `ociman rm`/`stop`/`restart`/`pause`/
`unpause --latest`/`-l` and `ociman images --sort` remain real,
confirmed, separate candidates for future increments.
