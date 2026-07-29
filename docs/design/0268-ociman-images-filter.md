# Design note 0268: `ociman images --filter`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_images.rs`.

## Closing the next real gap noted in `0267`

`ociman images` had no `--filter` at all, while real `podman images
--filter` — confirmed directly against a real installed `podman
images --help` — supports it, with its own worked example being
literally `podman images --filter dangling=true`. This note adds the
two most commonly used filters, `dangling=true|false` and `label=`/
`label!=`, both already implemented (and checked directly against
real podman) for `ociman prune --filter` (`docs/design/0192`,
`0266`'s own sibling feature) — this is a direct reuse of that
already-proven parsing and matching logic, not new design. Real
podman's own broader filter set (`reference`, `before`/`since`,
`until`, `id`, `digest`, `intermediate`, `readonly`, `manifest`,
`containers` — checked directly against
`vendor/go.podman.io/common/libimage/filters.go`) is real, larger
scope deliberately left for a later, similarly-scoped increment.

## Refactor: shared filter-value parsers

Rather than duplicating `ociman prune`'s own `label=`/`label!=`/
`dangling=` parsing logic verbatim for `images`, the per-filter-kind
parsing was factored out of `parse_prune_filters` into two small,
shared helpers used by both commands:

- `try_parse_label_filter(command, f)` — `None` if `f` isn't a
  `label=`/`label!=` value at all, `Some(Ok(LabelFilter))` or
  `Some(Err(_))` (malformed, e.g. empty key) otherwise.
- `try_parse_dangling_filter(command, f)` — same shape for
  `dangling=true|false`.

`parse_prune_filters` was verified byte-for-byte behavior-unchanged
after this refactor (all 25 pre-existing `ociman_prune.rs` tests still
pass unmodified), and a new, narrower `ImageFilters`/
`parse_image_filters` reuses the same two helpers for `ociman images`.
This guarantees the two commands' own `label=`/`dangling=` semantics
can never silently drift apart from each other in the future, the
same reasoning `LabelFilter` itself already existed for.

## Semantics

- `dangling=true` shows only untagged images (this project's own
  `is_untagged_reference` sentinel, `0179`); `dangling=false` shows
  only tagged ones — matching real `podman images --filter
  dangling=true` exactly.
- `label=<key>[=<value>]`/`label!=<key>[=<value>]`, OR'd together when
  more than one is given — the identical semantics and matching rule
  `ociman prune --filter label=` already established.
- An unrecognized filter kind is a clear, immediate error, matching
  `ociman prune`'s own identical rule for its own unsupported filters
  (never a silently-ignored no-op).
- Works with `--quiet`/`-q` and `--json` exactly like an unfiltered
  `ociman images` already does — the filter narrows which images are
  considered before either output mode renders them, not a separate
  code path.

## Verified

Integration (`tests/tests/ociman_images.rs`, four new tests):

- `--filter dangling=true` lists only the one real untagged image
  from a two-image store (one tagged, one built-on-top-and-untagged);
  `dangling=false` lists only the tagged one; the two result sets are
  confirmed disjoint.
- `--filter label=env=prod` lists only the one image with that exact
  label value; `label=env=staging` against the same image excludes it.
- An unrecognized filter kind (`before=...`) is a clear error.

Regression: all 25 pre-existing `ociman_prune.rs` tests and all 6
pre-existing `ociman_images.rs` tests (`-q`/`--quiet`, `docs/design/
0265`) still pass unmodified after the shared-parser refactor.

Full workspace: `cargo build`/`test --workspace`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`python3 ci/guards.py`, `cargo deny check`, `ci/native-ci.sh`,
`ci/build-deb.sh`.

## Still ahead

Real podman's own broader `images --filter` set (`reference`,
`before`/`since`, `id`, `intermediate`, etc.) remains a real,
similarly-scoped next candidate, as does `ociman rmi` gaining the same
`--filter`-driven bulk-removal shape `ociman prune`/`images` now both
share.
</content>
</invoke>
