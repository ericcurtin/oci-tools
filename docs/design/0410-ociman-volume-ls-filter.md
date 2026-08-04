# Design note 0410: `ociman volume ls --filter name=<substring>`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_volume.rs`,
`README.md`.

## What this closes

`ociman volume ls` had no `--filter` at all. Real `podman volume ls
--filter` supports a real grammar (`name=`/`driver=`/`label=`/
`label!=`/`opt=`/`until=`/`dangling=`/`after=`/`since=`); this closes
the first, smallest, most-used slice — `name=`.

## Real, checked-directly confirmation

`~/git/podman/pkg/domain/filters/volumes.go`'s own `GenerateVolumeFilters`:
`case "name": return func(v *libpod.Volume) bool { return util.
StringMatchRegexSlice(v.Name(), filterValues) }, nil` — a real regex
match against the volume's own name, the identical real primitive
`podman ps --filter name=`/`command=` already share (Go's own
unanchored `regexp.MatchString`, behaviorally a substring search for
ordinary, non-metacharacter text).

## Implementation

- `VolumeCommand::Ls` gains `filter: Vec<String>` (`--filter`).
- `cmd_volume_ls` parses each given value up front (only `name=` is
  recognized; anything else is a clear, immediate error naming the
  one supported key, matching this project's own established
  "not yet supported (only ... is)" convention every other `--filter`
  consumer already uses), then `Vec::retain`s the already-loaded
  record list by substring-matching each volume's own name against
  any one of the given values (OR'd together) — the same "avoid a new
  regex dependency" simplification `ociman ps --filter name=`/
  `command=` already established, applied to the identical real
  upstream primitive.

## Tests

Two new end-to-end integration tests in `tests/tests/ociman_volume.rs`:
`volume_ls_filter_name_matches_a_substring` (three real volumes, a
shared-prefix substring matching two of them, then two separate
`name=` values OR'd together matching a different pair, then a
non-matching substring finding nothing) and `volume_ls_filter_with_
an_unrecognized_key_is_a_clear_error`. All existing tests continue to
pass unmodified (34/34 in `ociman_volume.rs`).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures, clean on
the first full run), `python3 ci/guards.py`, `cargo deny check`,
`bash ci/native-ci.sh`, `bash ci/build-deb.sh` (real `dpkg -i`/
`--version`/`dpkg -r` round trip). This touches only `ociman volume
ls`'s own filter-matching path, not any hot path at all — no
benchmark re-run needed.

## Deliberately still out of scope

`driver=`/`label=`/`label!=`/`opt=`/`until=`/`dangling=`/`after=`/
`since=` remain unimplemented — `driver=`/`opt=` need a real driver-
option concept this project's fixed "local directory" volume model
doesn't have; `label=`/`label!=` need a real per-volume label schema
this project's volumes don't store at all yet; `until=`/`after=`/
`since=` need a real per-volume creation-time comparison (the
`VolumeRecord.created_at` field already exists, so this is the most
plausible next slice of this same flag, left for its own increment);
`dangling=` needs a real "is this volume currently referenced by any
container" concept this project's `containers_using_volume` helper
already computes for `rm`/`prune`, also left for its own increment
rather than folded into this one.
