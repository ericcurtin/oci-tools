# Design note 0411: `ociman volume ls --filter until=<duration-or-timestamp>`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_volume.rs`,
`README.md`.

## What this closes

Closes the "most plausible next slice" `0410`'s own "Deliberately
still out of scope" section flagged explicitly: `until=`/`after=`/
`since=` for `ociman volume ls --filter`, since `VolumeRecord.
created_at` already exists and this project already has the exact
shared `parse_until_filter_value` helper `ps`/`prune`/`images
--filter until=` established (`0407`). This note takes the smallest
of the three (`until=`) — `after=`/`since=` need resolving *another*
volume by reference first, a genuinely different mechanic left for
its own increment.

## Real, checked-directly confirmation

`~/git/podman/pkg/domain/filters/volumes.go`'s own
`createUntilFilterVolumeFunction`: `until, err := filters.
ComputeUntilTimestamp(filterValues)` (the exact same shared duration-
or-RFC3339 parser real podman's own `ps`/`images --filter until=`
already call through), then `v.CreatedTime().Before(until)` — a
strict "created before this real, absolute instant" comparison,
identical in shape to `filterBefore` (images) and this project's own
already-established `ps`/`prune`/`images --filter until=` semantics.

## Implementation

- `VolumeCommand::Ls`'s own `filter` field doc comment gains `until=`.
- `cmd_volume_ls` gains an `until_filter: Option<SystemTime>`,
  parsed via the already-shared `parse_until_filter_value` (refusing
  more than one value, matching real podman's own identical
  refusal), then `Vec::retain`s the record list by parsing each
  volume's own `created_at` (already a plain RFC3339 string) and
  keeping only those strictly before the threshold — a volume whose
  own `created_at` somehow isn't valid RFC3339 is excluded rather
  than erroring the whole listing, matching this project's own
  established "absence over fabrication" convention (`ociman images
  --filter before=`/`since=`'s own identical treatment).

## Tests

Two new end-to-end integration tests in `tests/tests/ociman_volume.rs`:
`volume_ls_filter_until_matches_volumes_created_strictly_before_the_
threshold` (a real, freshly created volume correctly excluded by a
`24h`-ago threshold, then correctly included once a `1s`-ago
threshold is given after a real 2-second sleep — the same real,
easy-to-get-backwards semantic `0407`'s own note already documents,
verified again here rather than assumed to carry over automatically)
and `volume_ls_filter_until_rejects_more_than_one_value`. All
existing tests continue to pass unmodified (36/36 in
`ociman_volume.rs`).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures, clean on
the first full run), `python3 ci/guards.py`, `cargo deny check`,
`bash ci/native-ci.sh`, `bash ci/build-deb.sh` (real `dpkg -i`/
`--version`/`dpkg -r` round trip). This touches only `ociman volume
ls`'s own filter-matching path, not any hot path at all — no
benchmark re-run needed.

## Deliberately still out of scope

`driver=`/`label=`/`label!=`/`opt=`/`dangling=`/`after=`/`since=`
remain unimplemented — `after=`/`since=` need a real "resolve another
named volume, then compare against its own creation time" mechanic
(the same shape `ociman images --filter before=`/`since=` already has
for images, but volumes have no equivalent resolver yet); the rest
each need real schema/data this project's volumes don't store at all
yet (matching `0410`'s own identical, still-unaddressed reasoning for
each).
