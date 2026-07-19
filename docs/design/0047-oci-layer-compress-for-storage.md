# Design note 0047: compressing a layer for storage, with its `diff_id` (milestone 4)

Status: implemented (compression + `diff_id` only — not yet wired into
anything)
Scope: `crates/oci-layer/src/compress.rs`.

0045 shipped the diff (`Vec<Change>`), 0046 shipped turning that into a
real uncompressed tar (`export`). Both of those notes' own "what's
still not here" flagged the same next gap: nothing yet produces the
two digests an image config/manifest actually need to record a newly
committed layer. This increment closes that gap: `oci_layer::
compress_for_storage`.

## Two digests, not one — checked directly against the image-spec, not assumed

An OCI layer has two separate digests, easy to conflate:

* **`diff_id`** — the digest of the layer's **uncompressed** tar
  content, recorded in the image config's own `rootfs.diff_ids` (OCI
  image-spec `config.md`). Identifies a layer's actual *content*,
  independent of how (or whether) it happens to be compressed.
* **The manifest's own layer descriptor digest** — the digest of the
  **compressed** bytes actually stored and transferred. This is simply
  whatever `oci_store::Store::ingest` computes on its own from the
  compressed bytes this increment hands it; `compress_for_storage`
  itself doesn't need to compute it (or depend on `oci-store` at all —
  see "Why `oci-layer` doesn't depend on `oci-store`" below).

`compress_for_storage(reader, writer)` streams an uncompressed tar
through a gzip encoder to `writer`, hashing the *uncompressed* side in
the same pass to produce the `diff_id` — one read of the input,
matching the same "hash while writing" streaming shape `oci_store::
Store`'s own `ingest_impl` already uses on the other end of this same
pipeline (checked directly against `crates/oci-store/src/lib.rs`, not
reinvented independently here).

## Compression level: `flate2::Compression::default()` (6), matching real moby's own default — not a project-specific choice

Real moby's own layer-export path (`compress/gzip`'s `gzip.NewWriter`)
uses Go's default compression level, which is also 6. Using the same
default here means a layer this project eventually builds compresses
to roughly the size (and takes roughly the time) a layer real `moby`
would produce for the same content — a fair, apples-to-apples
comparison for this project's own "beat upstream" goal, rather than an
arbitrary pick that could make an eventual `ociman build` look
artificially faster or slower than it really is relative to `docker
build`.

## Why `oci-layer` doesn't depend on `oci-store`

`compress_for_storage` only ever reads a stream and writes a stream —
it has no idea a blob store exists, and doesn't need to: the caller
(an eventual build-orchestration layer, not yet written) is expected
to hand the resulting compressed bytes to `oci_store::Store::ingest`
itself, exactly the same way `oci_registry::pull`'s own downloaded
compressed layer bytes already flow into the store today. Keeping this
one-way (compress here, store there) avoids giving `oci-layer` — whose
own charter, per its crate description, is tar application, not blob
persistence — a new dependency it doesn't actually need for anything
this increment does. `oci-layer` already gained a dependency on `oci-
spec-types` for its `Digest`/`Sha256Writer` types (a low-level types
crate several other crates already depend on for the same reason —
`oci-store`, `oci-registry`, `oci-runtime-core`, `oci-mount` — not a
new precedent).

## Real, automated tests

5 unit tests: the returned `diff_id` matches a plain `sha256` of the
raw uncompressed input (checked independently, not just "trust the
streaming code path"); the compressed output actually decompresses
back to the original bytes via `flate2::read::GzDecoder`; the same
input compressed twice yields the same `diff_id` (determinism); an
empty input produces the well-known empty-sha256 digest (`Digest::
empty_sha256()`); and the most convincing check — a real layer tar
produced by `crate::export` from a real captured diff, compressed by
this module, applied back through this crate's own existing `apply`
with `Compression::Gzip`, and the destination directory's own content
checked to match the source's mutated state exactly. That last test
exercises the complete diff → export → compress → apply path this
crate now supports end to end, using nothing but this crate's own
existing, already-tested primitives on both sides.

## Performance

Not called from anywhere yet (no `ociman build` CLI command exists to
call it) — zero runtime impact on any existing hot path by
construction, same reasoning every earlier not-yet-wired-in increment
in this Dockerfile/build pipeline (0039-0046) has used for itself.

## What's still not here

* Nothing yet drives `Store::ingest` with this module's own compressed
  output, or updates an image config's `rootfs.diff_ids` /
  history / a manifest's own layer list with the results — the actual
  "commit this RUN step as a new layer in the store" orchestration
  still doesn't exist as code anywhere.
* Everything else 0039-0046 already listed as future work: `ONBUILD`/
  `HEALTHCHECK`, `--build-arg`, the build cache, dependency-ordered
  execution actually running anything, and `ociman build` itself.
