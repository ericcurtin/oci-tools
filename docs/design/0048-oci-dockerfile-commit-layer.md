# Design note 0048: `commit_layer` — driving `Store::ingest` with a real diff (milestone 4)

Status: implemented (the store-ingest wiring only — not yet called
from any build executor, which doesn't exist yet)
Scope: `crates/oci-dockerfile/src/commit.rs`.

0045 (diff), 0046 (export), 0047 (compress) each shipped one link of
`oci-layer`'s own "turn a rootfs change into a real layer" chain, and
each one's own "what's still not here" named the exact same next gap:
nothing drives `oci_store::Store::ingest` with the result, and nothing
produces a manifest-ready `Descriptor`/`diff_id` pair from it. This
increment is that link: `oci_dockerfile::commit_layer`.

## Why this lives in `oci-dockerfile`, not `oci-layer` or `oci-store`

`oci-layer` deliberately stayed store-agnostic (0047's own design note
explains why: it only streams bytes, so a future build-orchestration
layer decides what to do with them). `oci-store` is a generic content-
addressed blob store used by three unrelated binaries (`ociman`
storage, `ocicri`'s image service, `ociboot`'s state partition) that
have no reason to know what a Dockerfile `RUN` step is. "Given a
diffed rootfs, commit it as a layer" is Dockerfile-build-specific
orchestration — and `oci-dockerfile`'s own module doc has said exactly
this since its first commit (0039): *"nothing here executes them yet
(that's `ociman build`'s own job, layered on top of this crate, `oci-
runtime-core` for `RUN` steps, and `oci-store` for layer commits)"*.
This increment is that named "`oci-store` for layer commits" piece,
landing in the crate that was always going to own it.

`oci-dockerfile` gains direct dependencies on `oci-layer`, `oci-store`,
and `oci-spec-types` — no cycle: none of those three depend back on
`oci-dockerfile` (checked via `cargo metadata` before touching
`Cargo.toml`, not assumed).

## What `commit_layer` actually does — three already-tested primitives, driven back to back, nothing new invented

```rust
pub fn commit_layer(
    store: &Store,
    root: &Path,
    changes: &[Change],
) -> Result<CommittedLayer, CommitLayerError>
```

1. `oci_layer::export(root, changes, &mut tar_bytes)` (0046) — the
   diff, turned into a real uncompressed tar.
2. `oci_layer::compress_for_storage(&tar_bytes, &mut compressed)`
   (0047) — gzip-compressed, with the uncompressed content's own
   `diff_id` computed in the same pass.
3. `store.ingest(&compressed)` — the compressed bytes persisted
   content-addressedly, returning the digest/size that become the new
   layer's manifest `Descriptor`.

Returns a `CommittedLayer { descriptor: Descriptor, diff_id: Digest }`
— exactly the two pieces a caller needs to append to a manifest's own
`layers` list and an image config's own `rootfs.diff_ids` list,
respectively (in the same relative order in both, per the image-spec).
No intermediate step (the tar bytes, the compressed bytes) is exposed
for a caller to accidentally reorder or skip.

## An empty diff still commits a real layer — matches real BuildKit, not special-cased away

A `RUN` step that happens to touch nothing on disk (or an explicitly
empty `changes` list) still produces a real, valid, if degenerate,
layer (an empty tar archive — still two 512-byte zero blocks per the
tar format, not zero bytes) rather than being skipped by
`commit_layer` itself. Real `moby`/BuildKit do the same — whether an
empty-diff layer is even worth keeping (vs. collapsing it into a
no-op `history` entry with `empty_layer: true` and no corresponding
`rootfs.diff_ids` entry, the way `ENV`/`LABEL`/`CMD` instructions are
recorded) is a build-executor's own policy decision, not something
this narrow a function should decide on a caller's behalf.

## Real, automated tests

3 unit tests: a real diff (an added file under a new subdirectory)
committed and read back from the store, cross-checked against
independently re-running `export`/`compress_for_storage` on the same
diff to confirm `ingest` is a pure pass-through of already-compressed
bytes rather than some reprocessing step of its own; an empty change
list still produces a real, non-zero-size stored layer; and two
separately committed diffs from the same rootfs (captured at two
different points) get independent, content-addressed digests, both
retrievable from the store afterward.

## Performance

Not called from anywhere yet (no build executor exists to call it, no
`ociman build` CLI command exists) — zero runtime impact on any
existing hot path by construction, same reasoning every earlier not-
yet-wired-in increment in this Dockerfile/build pipeline (0039-0047)
has used for itself.

## What's still not here

* Nothing decides *when* to diff a rootfs (before/after which
  instruction), or drives a `RUN` step via `oci-runtime-core` in the
  first place — a build executor loop, still entirely unwritten.
* Nothing updates an image config's own `rootfs.diff_ids`/`history` or
  a manifest's own `layers` list with `commit_layer`'s own output —
  trivial glue (`Vec::push` twice) once a caller exists to do it, but
  no such caller exists yet.
* Everything else 0039-0047 already listed as future work: `ONBUILD`/
  `HEALTHCHECK`, `--build-arg`, `COPY --from=<stage>` dependency
  resolution, the build cache, and `ociman build` itself.
