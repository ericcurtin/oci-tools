# Design note 0274: `ociman run`/`create --label`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `bin/ociman/src/build.rs`,
`tests/tests/ociman_inspect.rs`.

## A prerequisite gap found while researching `ps --filter label=`

While investigating `0272`/`0273`'s own deferred `ociman ps --filter
label=` candidate, careful re-verification of a genuinely confusing
earlier test result (see below) led to discovering a real, more
fundamental gap first: `ociman run`/`ociman create` had **no `--label`
flag at all** — containers in this project carried no labels of their
own whatsoever, only images did (`ociman build --label`, `0135`).
Real `podman ps --filter label=` filters on a *container's* own
labels (`c.Labels()`), so implementing that filter honestly needs
container-level labels to exist first. This note closes that
prerequisite; `ps --filter label=` itself remains for a follow-up
increment now that there's something real to filter on.

## Resolving the `ps --filter label=` "mystery" from `0272`

`0272`'s own research note flagged a confusing real result: `podman ps
--filter label=env=prod --filter label=env=staging` appeared to match
*both* of two containers, neither a clean AND nor OR reading fully
explained. Redone carefully from a verified-clean slate this time
(fresh containers, labels double-checked via `podman inspect`
immediately beforehand), the result was unambiguous: **empty** —
exactly matching a straightforward AND reading of real podman's own
`filters.MatchLabelFilters` (every given `label=` value must be
satisfied by *some* label on the same container). The earlier
confusing result was test contamination from incompletely cleaned-up
state across repeated manual test iterations in that session, not a
real behavioral anomaly. Re-verified cleanly end to end: a single
`label=` value matches correctly, two *jointly satisfiable* values
both matching the one container that has both still match, and two
*jointly unsatisfiable* values correctly find nothing.

This confirms a genuine, legitimate divergence from `ociman prune
--filter label=`'s own already-shipped OR semantics (`0192`): real
podman's own `image`-level label filtering (`libimage/filters.go`'s
`filterLabel`, one separate `filterFunc` *per value*, later OR'd by
the image-filter-compilation layer) is a **different real function**
than its `container`-level one (`pkg/domain/filters/containers.go`'s
`GenerateContainerFilterFuncs`, one call to `MatchLabelFilters` with
*every* value at once, ANDed internally) — not a project
inconsistency to resolve, but two genuinely different pieces of real
upstream behavior this project should (and, once `ps --filter label=`
itself lands, will) faithfully mirror each on its own terms.

## Real semantics implemented here, checked directly

- `--label KEY=VALUE`, or bare `KEY` for an empty value (repeatable) —
  matching real `docker run --label`/`podman run --label` exactly,
  reusing `ociman build --label`'s own already-established, identical
  tolerant parser (`build::parse_key_value_pairs`, now `pub(crate)`
  rather than duplicated).
- A container with **no** explicit `--label` still shows its base
  image's own real `LABEL`s via `ociman inspect`'s own `labels` field
  — verified directly: a real `podman create` with no `--label` at
  all, against an image with its own `LABEL`, showed that label in
  `podman inspect`'s own `Config.Labels`.
- An explicit `--label` **merges** with (rather than replacing) that
  inherited set, a same-key `--label` overriding the image's own
  value — also verified directly.
- Available on both `ociman run` and `ociman create` for free (both
  share the same `RunArgs`/`prepare_container` — `0157`'s own already-
  established flatten pattern), no separate wiring needed for either.

## Storage

The container's own real, effective label map (image-inherited plus
`--label` merged in) is stored as a single JSON-encoded value under
one new annotation, `io.oci-tools.labels`, rather than one annotation
per label key — this project's own `annotations` map already has
real, established keys of its own (`io.oci-tools.image`/`.name`/...)
that a namespaced-per-label-key scheme risks colliding with, and a
container's label set is naturally read/written as one whole map
anyway (`ociman inspect`'s own new `labels` field, `BTreeMap<String,
String>`). A container predating this field entirely (no annotation
recorded) reports a real, honest empty map rather than an error.

## Verified

Integration (`tests/tests/ociman_inspect.rs`, two new tests):

- A container created with no `--label` at all shows its base image's
  own real `LABEL` via `ociman inspect`'s own `labels` field.
- `--label own.label=fromcli --label barekey --label shared.key=
  fromcli` against an image with its own `image.label`/`shared.key`
  labels merges correctly: the image-only label survives untouched,
  the shared key is overridden to the CLI's own value, the bare-key
  flag becomes an empty string, and the CLI-only label is added.

Full workspace: `cargo build`/`test --workspace`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`python3 ci/guards.py`, `cargo deny check`, `ci/native-ci.sh`,
`ci/build-deb.sh`.

## Still ahead

`ociman ps --filter label=`/`label!=` is implemented in `0275`.
`--label-file <path>` (reading additional labels from a file, real
podman/docker's own sibling flag to `--label`) remains a further,
smaller candidate.
</content>
</invoke>
