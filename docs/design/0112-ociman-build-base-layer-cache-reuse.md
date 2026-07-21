# Design note 0112: `ociman build` reuses the rootfs cache for its own base layer

Status: implemented
Scope: `bin/ociman/src/build.rs` (`build_stage`, `cmd_build`,
`clone_cache_tree` new); `bin/ociman/src/rootfs_setup.rs`
(`decide`/`prepare_overlay` signature change: `manifest: &ImageManifest`
→ `layers: &[Descriptor]`); `bin/ociman/src/main.rs` (call site update);
`crates/oci-store/src/rootfs_cache.rs` (`ensure_cached` signature
change, same reason).

## Why this, now

0106-0110 found and closed a real `ociman run` startup-time gap by
reusing a per-manifest-digest extracted-rootfs cache instead of
re-running `oci_layer::apply` over every base layer on every
invocation. `ociman build` has the exact same unconditional
per-invocation extraction cost in `build_stage`'s own base-layer setup
(`for layer in &layers { oci_layer::apply(...) }`) — never gated by any
cache at all — and nothing in 0106-0111 ever touched it.

Measured directly, not assumed, before touching any code: a real
`FROM docker.io/library/ubuntu:24.04` + `RUN echo hello > /marker.txt`
Containerfile, `hyperfine`, both images already pulled/cached locally
(a fair "warm" comparison, not one side paying registry-pull cost the
other doesn't):

| | before this change | `podman build` (warm) |
|---|---|---|
| mean | ~247.9 ms | ~87.3 ms |

`podman build`'s own warm path was ~2.8× faster — the same shape of
regression 0107 found for `ociman run`, just never checked for here.

## The fix: copy from the rootfs cache instead of re-extracting

`build_stage`'s scratch rootfs setup now takes an
`Option<&Digest>` (`base_manifest_digest`, threaded from `cmd_build`'s
own base-layer resolution — `Some` only when this stage's base is a
real external image just pulled/resolved via `resolve_or_pull`, `None`
for `FROM <earlier-stage>`, which never had a manifest digest of its
own to begin with):

* `Some(digest)`: `oci_store::ensure_cached(store, &cache_root, digest,
  &layers)` (the exact same cache `ociman run` already builds and
  reuses since 0109/0110 — same manifest digest always means the same
  fully-extracted content, so there is nothing image-specific about
  reusing it from a different command), then a plain recursive copy
  (`clone_cache_tree`, new) into the stage's own fresh scratch rootfs.
* `None`: the original per-layer `oci_layer::apply` loop, unchanged —
  an in-memory earlier-stage result has no cache entry to reuse.

An overlay mount (0110's own approach for `ociman run`) was
deliberately *not* used here instead: a build's own rootfs has to stay
writable across however many further `RUN`/`COPY` instructions the
stage has, for as long as the whole (possibly multi-stage) build keeps
running — a lifetime overlay's own upper/lower/work-dir bookkeeping
isn't a good fit for, and multi-stage/writable-rootfs interaction was
already flagged as out of scope for overlay when 0110 was written. A
plain, real recursive copy is a much smaller, safer increment that
still avoids the actual dominant cost (re-decompressing and re-parsing
every base layer's own tar stream on every single build).

### `clone_cache_tree`: real fidelity, not `copy_path_recursive` reused as-is

This project already has a recursive copy helper
(`copy_path_recursive`, used by `COPY`/`ADD`) — reused for the actual
byte copying, but *not* for directory permissions: `copy_path_recursive`
only sets a directory's mode when an explicit `--chmod` override is
given (correct for its own use case, where `COPY`'s destination
directories not otherwise touched keep an ordinary default mode).
Reusing it as-is here would have silently dropped every directory's
own real mode from the image — including well-known real cases like
`/tmp`'s common `1777`. `clone_cache_tree` is a new, small, dedicated
function instead: always preserves the source's own mode, directories
included; never `chown`s anything (`oci_layer::apply` itself never does
either — see its own module doc comment — so both sides of the copy
are already owned by the same real calling user regardless).

Caught by `strace`, not assumed: an early version also unconditionally
tried `remove_file` before creating every symlink (copied verbatim from
`copy_path_recursive`, where a pre-existing destination from an earlier
`COPY` in the same stage is a real possibility). For `clone_cache_tree`
the destination is always somewhere inside a scratch rootfs this same
call just created fresh, so that removal is *always* a guaranteed-ENOENT
`unlinkat` — confirmed exactly (194 failing `unlinkat` calls, exactly
matching the real symlink count in a `busybox`-style layer) and removed.

## Real, automated tests

Three new direct unit tests for `clone_cache_tree` (`bin/ociman/src/
build.rs`): a plain file's own content and mode (`0640`) survive the
clone; a directory's own unusual mode (`01777`, the real `/tmp` case)
survives (would fail against a naive `create_dir_all`-only
implementation); a symlink stays a real symlink, never dereferenced.
All 44 existing `ociman build` integration tests
(`tests/tests/ociman_build.rs`) still pass unmodified — several of them
already exercise a real external-image `FROM` (e.g.
`builds_a_metadata_only_image_and_applies_every_supported_instruction`,
`run_executes_a_real_command_and_commits_a_real_new_layer`), so they
now exercise the new cache-copy path end to end, not just the unit
tests in isolation. Full workspace `cargo test --workspace --locked`
run clean five times in a row this session (no flakiness).

## Measured result

Same benchmark as above, after this change, both images still already
pulled/cached locally:

| | before | after this change | `podman build` (warm) |
|---|---|---|---|
| mean | ~247.9 ms | **~93.6-101.9 ms** | ~83.6-87.4 ms |

A real, large improvement (~2.5-2.6× faster than before) — closing
almost all of the previously-measured ~2.8× gap against `podman build`
down to roughly **1.2×** (noisy but consistently reproducible across
repeated back-to-back `hyperfine` comparisons; not fully closed, see
below).

## What this doesn't do yet

* The remaining ~1.2× gap against `podman build` was investigated (not
  just assumed away): `strace -c` on a warm `ociman build` shows most of
  the remaining syscall time split between the new recursive-copy pass
  itself and the *final* recursive removal of the stage's own scratch
  rootfs once the build finishes (a real `TempDir::drop` /
  `remove_dir_all` over every file the copy just wrote) — both `O(n)` in
  the base image's own file count. `podman build`'s own warm path uses
  an overlay-mounted build container, where both the equivalent setup
  and teardown are `O(1)` (a mount and an unmount) regardless of image
  size. Closing this fully would mean giving `ociman build` its own
  overlay-based rootfs lifecycle — a genuinely bigger, riskier change
  (multi-stage and writable-rootfs interaction with overlay was already
  flagged as unresolved when 0110 was written) intentionally left for a
  future, separately-scoped increment rather than rushed into this one.
* `COPY --from=<external-image>`'s own separate extraction path
  (`external_image_source_root`) is untouched — narrower in scope than
  a stage's own base layers (only ever needs the one `COPY`'s own
  source files, not a whole writable rootfs kept alive for a whole
  stage), and not measured as a comparable cost here; left for a future
  increment if it ever shows up as one.
