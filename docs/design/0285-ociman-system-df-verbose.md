# Design note 0285: `ociman system df -v`/`--verbose`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_system_df.rs`.

## Closing 0263's own "still ahead"

`docs/design/0263` shipped `ociman system df`'s default summary table
but deferred both `-v`/`--verbose` and the precise per-image
"unique vs. shared across other stored images" size split (it used a
simpler, honest, narrower-undercount approximation for the summary's
own `reclaimable` column instead). This slice adds `--verbose`, and in
doing so implements the real cross-image blob-reference-count pass
that 0263 explicitly deferred — checked directly against real
`podman system df -v`'s own shape from `~/git/podman/cmd/podman/
system/df.go`'s `printVerbose` function.

Real podman's own `-v` **replaces** the summary table entirely with
three headed sections (no aggregate shown alongside it) — this project
matches that: `--verbose` shows only the per-item breakdown, not the
existing summary.

| Section          | Real podman columns                                                              |
| ---------------- | --------------------------------------------------------------------------------- |
| Images            | REPOSITORY / TAG / IMAGE ID / CREATED / SIZE / SHARED SIZE / UNIQUE SIZE / CONTAINERS |
| Containers        | CONTAINER ID / IMAGE / COMMAND / LOCAL VOLUMES / SIZE / CREATED / STATUS / NAMES  |
| Local Volumes     | VOLUME NAME / LINKS / SIZE                                                        |

## The real shared/unique computation

This project's own store is content-addressed (every blob is already
keyed by its own digest), which makes the real answer straightforward
rather than an approximation:

1. Collect one representative record per *distinct* stored image (by
   manifest digest — two tags of the same image share one entry here).
2. For each distinct image, read its own manifest's config + layer
   blob digests (already-existing `Store::image_manifest`).
3. Build a `blob digest -> set of referencing image manifest digests`
   map across every distinct image.
4. For each distinct image, sum blob sizes into `shared` (referenced
   by more than one distinct image) vs. `unique` (referenced by
   exactly one) — a real computed split, not an approximation.
5. Emit **one row per real reference/tag** (matching `ociman images`'
   own established one-row-per-tag convention, not deduplicated), with
   every tag of the same underlying image reporting the identical
   precomputed shared/unique split and container count.

Verified directly with two synthetic images that share the same real
busybox+applets layer (byte-identical tar, so the same real layer
digest) but differ in their own `ContainerConfig` (`Cmd`), which
gives them genuinely different config blobs and therefore different
manifest digests — two real, distinct images that share one real
layer. Both report a non-zero `shared_size_bytes`.

## Simplifications, documented honestly

- **`created`**: the image's raw RFC3339 `ImageConfig.created` string
  is shown as-is, not converted to a human-relative duration (matching
  this project's own `ociman ps` CREATED-column convention already
  established elsewhere, rather than real podman's own relative-time
  formatting). Note this is the *image's* own build-time creation
  timestamp (`ImageConfig.created`), not to be confused with
  `ImageManifest`'s own per-history-entry `created`/`created_by`
  fields, which describe individual build layers, not the image as a
  whole.
- **Container `LOCAL VOLUMES`**: counts how many of the container's own
  bundle `spec.mounts` entries have a `source` matching one of this
  store's own named-volume data directories — real, not approximated.
- **Volume `LINKS`**: reuses the exact same `containers_using_volume`
  helper the existing summary's own `active`/`reclaimable` volume
  columns already use.
- **`--json` composes with `--verbose`**: real podman refuses to
  combine `-v`/`--verbose` with its own `--format`. This project's own
  `--json` is a global flag (not a per-command one the way podman's
  `--format` is), and composes with `--verbose` just fine — a real,
  deliberate divergence, not an oversight.

## Verified

Integration (`tests/tests/ociman_system_df.rs`, five new tests):

- A single stored image's verbose row reports the correct
  repository/tag, and (with no other stored image to share a layer
  with) its entire size counts as `unique_size_bytes` with
  `shared_size_bytes` at zero.
- Two distinct images (different manifest digest, via a different
  `Cmd`) that share one real layer (identical applets) both report a
  non-zero `shared_size_bytes` for the shared blob.
- A container created with `-v name:/data` reports `local_volumes: 1`.
- The named volume it mounts reports `links: 1`.
- The plain-text `--verbose` output shows all three real headed
  sections and their real column headers.

Full workspace: `cargo build`/`test --workspace` (110 test binaries),
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets --
-D warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh`.

## Still ahead

`--format` (real podman's own Go-template output format flag) remains
unimplemented for both the summary and verbose shapes.
