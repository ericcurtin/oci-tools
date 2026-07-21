# Design note 0109: a real, tested per-manifest-digest rootfs cache primitive

Status: implemented (groundwork only — see "What this doesn't do yet")
Scope: `crates/oci-store/src/rootfs_cache.rs` (new module,
`cache_dir_for`/`ensure_cached`), `crates/oci-store/src/lib.rs`
(module registration, `StoreError::UnsupportedLayerMediaType`),
`crates/oci-layer/src/lib.rs` (new shared `compression_for_media_type`),
`bin/ociman/src/main.rs` (its own equivalent now delegates to the
shared one). No existing behavior changes.

## Why this, now

0108 landed the *other* missing piece (a real, tested rootless-overlay
feasibility probe) and named this exact primitive — a per-image-
manifest-digest "golden" cache of an already-extracted rootfs — as the
one still needed before a future increment can safely wire a real
overlay-based rootfs into `ociman run`, closing 0107's own documented
gap (real `podman run` now 1.71× faster than `ociman run` for a real
multi-thousand-file image, because `ociman` still fully extracts every
layer's own files from scratch on every single invocation). This
session builds that second piece, the same way: as its own safe,
fully-tested, standalone increment, landed *ahead of* actually wiring
it into any live container path.

## What it does, and what it deliberately doesn't decide

`ensure_cached(store, cache_root, manifest_digest, manifest)`: build a
fully-extracted rootfs at a deterministic, content-addressed path
(`cache_root/<manifest-digest-hex>/`) the first time a given manifest
digest is ever needed, reusing it on every later call for the same
digest. The extraction itself is exactly the loop `ociman run`'s own
current, uncached code already runs (`oci_layer::apply`, once per
layer) — nothing new there; the only new work is *where* it writes
(a shared cache directory instead of one throwaway per-container
directory) and *when* it's skipped entirely (any digest already
cached).

**Concurrency-safe by construction, not by locking**: the real build
happens inside a fresh temp directory under `cache_root` (guaranteeing
the eventual `rename` is same-filesystem and atomic), which only ever
becomes visible at the real cache path via one `rename(2)`. A losing
concurrent caller's own redundant build is simply discarded — safe to
do cheaply, without any lock file, precisely because losing the race
is harmless: the same manifest digest can only ever mean the same real
layer content, so the winner's own result is byte-for-byte identical
to what the loser would have produced.

**Deliberately does not decide how a container ends up seeing this
cache.** As `oci_layer::apply`'s own doc comment section on ownership
already explains for a different reason, and 0108's own module doc
explains for this exact cache: sharing it via a plain recursive copy
defeats the entire point (still pays the same per-file write cost this
module exists to avoid paying more than once); sharing it via
hardlinks is actively unsafe (a write inside any one container would
silently corrupt the shared cache for every other container of the
same image, since a hardlink is the same inode, not an independent
copy). Real safety needs a real copy-on-write layer between a
container and this cache — a real overlay mount, this cache's own
output as `lowerdir` — which is exactly what 0108's own probe exists
to help decide is safe to attempt, and what a future increment still
needs to actually wire in.

## Where this lives, and why

Needed both `oci_store::Store` (to read layer blobs back out) and
`oci_layer::apply` (to extract them) — neither crate previously
depended on the other. Two real options were weighed: extend
`oci-layer` (whose own doc comment scopes it narrowly to "applying one
image layer... onto a root filesystem directory," a poor fit for
"manage a store-backed extraction cache"), or extend `oci-store`
(already owns "here's where different kinds of image-related state
live on disk" — blobs, image pointers — a natural fit for "and a
derived, content-addressed rootfs-extraction cache" too). Went with
`oci-store` gaining a new, forward-only dependency on `oci-layer`.

This also surfaced a real, small duplication worth fixing regardless:
`ociman`'s own `compression_for_media_type` (media type ->
`oci_layer::Compression`) had no shared home either. Moved the mapping
itself into `oci_layer::compression_for_media_type` (a pure function,
now used by both `oci_store`'s new cache module and `ociman`'s own
existing call sites, which keep their established `anyhow`-flavored
wrapper unchanged) — one implementation, not two, matching this
project's own "share as much Rust code as possible" standard directly,
not just in spirit.

## Why library code, not a `bin/ociman`-only module

An earlier draft placed this directly in `bin/ociman` (matching how
0101's own build cache, `build_cache.rs`, is deliberately `ociman`-only
— this concept genuinely has no use in `ocirun`, which has no image/
layer concept at all, matching its own "spec-driven only, no policy"
design pillar). That draft failed `cargo clippy --workspace --all-
targets -- -D warnings` outright: an unused `pub fn` in a *binary*
crate is real dead code (`-D dead-code`) the moment nothing outside
its own tests calls it yet, unlike an unused `pub fn` in a *library*
crate (assumed part of the crate's own external API surface, exempt
from that lint) — exactly the reason 0108's own `oci_runtime_core::
overlay` module could land unwired without this problem, and this
session's own equivalent needed the same treatment: real library code
in `oci-store`, not a binary-only module, so it can land safely ahead
of being wired into any real command.

## Real, automated tests

Four, all against a real, offline-seeded single-layer manifest (a real
gzip-compressed tar ingested directly into a temp `Store`, the same
"structurally real, no registry needed" pattern this workspace's own
test suite already establishes elsewhere): a first call actually
extracts real file content; a second call for the *same* digest
reuses the existing directory without rebuilding (checked by mutating
the cache directly between calls and confirming the mutation survives
a second `ensure_cached` call — if it silently rebuilt, this would
fail); two different digests get two independent, non-colliding cache
directories; and no stray temporary build directory is left behind
once a real build completes.

## What this doesn't do yet

* **Nothing in `ociman run`'s own live container path calls this
  yet** — same "pure groundwork first" scoping 0108 already
  established, for the same reason: the actual wiring (building/
  reusing this cache as an overlay's own `lowerdir`, a fresh per-
  container `upperdir`/`workdir`, one new `mounts` entry in the
  bundle's own `config.json`, a graceful fallback to today's per-
  container extraction when 0108's own probe says overlay isn't safe
  here) is real, correctness-sensitive surface (concurrent cache
  population already handled here, but also `--read-only` interaction,
  and `ociman run`'s own `resolve_user` currently reading `/etc/passwd`
  directly off the *container's own* rootfs directory before the
  container ever starts — which would need to read from *this cache*
  instead once the container's own rootfs directory stops being
  populated directly) best landed as its own dedicated, carefully
  tested increment, not rushed into the same session as this
  primitive.
* Nothing cleans up an old/unreferenced cache entry — matches this
  project's own existing `Store::gc`'s own scope (blobs/images only)
  precisely, and is deferred here for the identical reason: a future
  `gc`-equivalent command doesn't exist for this cache yet either,
  same open item 0102's own design note already flagged for blob
  garbage collection generally.
* `ociman build`'s own analogous base-layer extraction
  (`build_stage`) doesn't use this cache either — its own rootfs gets
  further modified (`RUN`/`COPY`/`ADD`) and re-diffed to produce new
  layers, a materially different, more invasive change to wire up
  (the diff computation would need to understand an overlay's own
  `upperdir` as the diff directly) than `ociman run`'s own simpler
  "extract once, run one command, tear down" shape — left as
  previously-noted future work, unchanged by this increment.
