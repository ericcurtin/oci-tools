# Design note 0350: `ociman images --filter digest=sha256:<prefix>`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_images.rs`.

## What this closes

`0349`'s own "still ahead" list flagged `digest=` as the natural next
candidate after `id=`, deliberately deferred at the time pending a
closer look at what real podman's own multi-digest semantics should
mean for this project's own, simpler image model.

## Real, checked-directly semantics

Read `~/git/container-libs/common/libimage/filters.go`/`image.go`
directly:

- `filterDigest(value)`: requires `value` to start with `sha256:`
  outright (`if !strings.HasPrefix(value, "sha256:") { return nil,
  fmt.Errorf(...) }`) — a real, immediate parse-time error, unlike
  `id=` (which matches a bare hex ID and has no such requirement).
- The match itself, `containsDigestPrefix`, checks every digest the
  image is "known by" (`img.Digests()`), not just one: `for _, d :=
  range i.Digests() { if strings.HasPrefix(d.String(),
  wantedDigestPrefix) { return true } }`.
- Traced `Digests()` down to `containers/storage`'s own
  `recomputeDigests` (`~/git/container-libs/storage/images.go`): it's
  the image's own canonical digest *plus* every additional digest
  recorded under a `"manifest"`-prefixed "big data" name — which only
  ever has more than one entry for a real multi-arch, fat-manifest-
  list pull (each per-platform manifest gets its own separate
  big-data-with-a-manifest-prefixed-name entry alongside the list's
  own). For an ordinary, single-platform pull (this project's own
  *only* case — every image here is resolved to exactly one platform
  at pull/build time, `0307`), `Digests()` reduces to exactly one
  entry: the canonical manifest digest itself.
- Combination rule: same generic, per-key AND-everything-together
  default `id=` already established (`0349`) — `digest=` isn't one of
  the explicit OR exceptions (`reference=`'s dedicated handling,
  `dangling=`'s single-value rule) either.

## Design decision

Given this project's own image-storage model genuinely has no
fat-manifest-list concept at all (confirmed by tracing `Digests()`'s
real source, not assumed), its own single `manifest_digest` per image
*is* the complete, faithful equivalent of real podman's own
`Digests()` for the case that actually matters here — there's no
narrower or approximate feature to build, no missing multi-digest
tracking to add first. `0349`'s own deferral was warranted (needed
this trace to confirm, not just assume, the single-digest reduction
holds) but the actual feature ends up exactly as small and bounded as
`id=` once confirmed.

## Implementation

New `ImageFilters::digest: Vec<String>` field. Parsed in
`parse_image_filters`: `strip_prefix("digest=")`, then a real,
immediate error if the remaining value doesn't itself start with
`sha256:` (matching `filterDigest`'s own validation exactly — checked
*before* the value is ever stored, not deferred to filter-application
time). Applied in `cmd_images`'s per-record loop as `filters.digest.
iter().all(|prefix| record.manifest_digest.to_string().starts_with
(prefix))` — `.to_string()` (not `.hex()`, unlike `id=`) since the
value being matched against is the full `sha256:<hex>` string, not a
bare hex one; `.all`, not `.any`, for the same AND-not-OR reason `id=`
already established.

## Verified

New tests in `ociman_images.rs`:
`images_filter_digest_matches_by_full_digest_string_prefix` (the exact
full digest, and a shorter genuine prefix of it, both checked),
`images_filter_digest_with_two_different_values_matches_nothing` (the
AND-not-OR consequence, mirroring `id=`'s own identical test),
`images_filter_digest_without_sha256_prefix_is_a_clear_error`.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (113 test-result blocks,
0 failures), `python3 ci/guards.py`, `cargo deny check`.

## Still ahead

`readonly=`/`intermediate=`/`manifest=` remain separate, not-yet-
scoped candidates from `0349`'s own survey — `intermediate=`/
`manifest=` both need real computations (a parent/child layer-tree
query; a "is this a manifest list" concept) this project doesn't build
at all yet, genuinely bigger than either `id=`/`digest=` turned out to
be.
