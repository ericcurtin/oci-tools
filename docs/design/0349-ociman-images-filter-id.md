# Design note 0349: `ociman images --filter id=<prefix>`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_images.rs`.

## What this closes

`0268`'s own "still ahead" note explicitly named real podman's own
broader `images --filter` set (`reference`, `before`/`since`, `id`,
etc.) as a real, similarly-scoped candidate. `reference=`/`before=`/
`since=` were all closed in later increments; `id=` was the one
genuinely still-missing kind left, confirmed by a fresh survey this
turn (which also ruled out `network`/`pod` subsystems as out-of-scope
big features, and `ocicri`'s remaining `Exec`/`Attach`/`PortForward`
RPCs as needing a whole separate streaming-server architecture, not a
small follow-on).

## Real, checked-directly semantics

Read `~/git/container-libs/common/libimage/filters.go` directly:

- `filterID(value)`: `strings.HasPrefix(img.ID(), value)` — a prefix
  match against the image's own full manifest digest (hex, no
  `sha256:` prefix; real podman's own `Image.ID()`).
- Combination rule, checked directly via `applyFilters`
  (`compiledFilters map[string][]filterFunc`, iterated as "all filters
  of each key must apply"): `id=` is **not** one of the two explicit
  exceptions this file's own `compileImageFilters` switch carves out
  (`reference=`'s own dedicated OR-then-AND handling; `dangling=`'s
  single-value-only rule) — it falls through to the plain, generic,
  AND-everything-together default every other simple filter kind
  shares. A real, checked-rather-than-assumed consequence: two
  different `id=` values given together match nothing at all (no
  single image's ID can start with two different prefixes
  simultaneously), not their union — genuinely different from this
  project's own `label=`/`reference=` filters, both of which are
  deliberately OR'd within their own key (`0192`/`0268`'s own already-
  established, different combination rule for those two specifically).

## Implementation

New `ImageFilters::id: Vec<String>` field; parsed in
`parse_image_filters` the same shape `before=`/`since=`/`reference=`
already use (`strip_prefix("id=")`, reject an empty value). Applied in
`cmd_images`'s own per-record filter loop as `filters.id.iter().all(|
prefix| record.manifest_digest.hex().starts_with(prefix))` — `.all`,
not `.any`, is the one line that actually encodes the AND-not-OR rule
above; no new store lookups, no new concept, `record.manifest_digest`
is already read for every other filter kind in the same loop.

## Verified

New tests in `ociman_images.rs`:
`images_filter_id_matches_by_manifest_digest_prefix` (matches by the
exact short digest `-q` itself prints, and by a shorter, genuine
prefix of it too — confirming this is a real prefix match, not an
exact-string one), `images_filter_id_with_two_different_values_
matches_nothing` (the AND-not-OR consequence above, proven with two
real, distinct images), `images_filter_id_missing_a_value_is_a_clear_
error`.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (113 test-result blocks,
0 failures), `python3 ci/guards.py`, `cargo deny check`.

## Still ahead

`digest=sha256:<prefix>` (matches any of an image's own *known*
digests, not just its primary manifest digest — real podman's own
`img.Digests()` covers multi-arch fat-manifest-list membership this
project's own single-platform-resolved-at-pull-time image model
doesn't track the same way, so faithfully porting this needs a closer
look at what it should mean here before implementing it, rather than a
same-shape copy of `id=`) and `readonly=`/`intermediate=`/`manifest=`
(the latter two needing a real parent/child layer-tree computation
this project doesn't build at all) remain separate, not-yet-scoped
candidates from the same survey. `ocicri`'s remaining `Exec`/`Attach`/
`PortForward` RPCs are confirmed to need a whole separate streaming-
server architecture (real cri-o's own `k8s.io/cri-streaming` package)
this project has no equivalent of — a genuinely bigger item, not
picked up here.
