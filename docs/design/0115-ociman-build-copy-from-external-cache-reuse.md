# Design note 0115: `ociman build`'s `COPY --from=<external-image>` reuses the rootfs cache too

Status: implemented
Scope: `bin/ociman/src/build.rs` (`external_image_source_root`,
`copy_instruction`'s own call site)

## Why this, now

0112 fixed the exact same unconditional-per-invocation-extraction shape
for a stage's own *base* layers; `external_image_source_root` (backing
`COPY --from=<external-image>`) had the identical bug and was
explicitly named in 0112's own "what this doesn't do yet" section as
left for later. Every call re-extracted an external image's entire
layer stack into a fresh, throwaway `tempfile::TempDir`, discarded once
that one `COPY` finished — even when the *same* external image is used
as a `--from=` source more than once, in one build or across several
separate ones.

## The fix is simpler than 0112's own: no copy needed at all

Unlike a stage's own base rootfs (`build_stage`, genuinely needs a
*writable* directory kept alive across however many further `RUN`/
`COPY` instructions the stage has), a `COPY --from=<external-image>`'s
own source root is **read-only, single-use, and short-lived** —
`copy_instruction`'s own `copy_path_recursive` calls only ever read
`source_path` (under `source_root`) and write to `target` (in the
*stage's* rootfs), never the other way around. That means
`external_image_source_root` doesn't need 0112's own `clone_cache_tree`
copy step at all: it can return `oci_store::ensure_cached`'s cache
directory path directly and let `COPY` read straight out of it — the
exact same safe "shared, read-only, never written to" usage `ociman
run`'s own overlay `lowerdir` (0110) already established for this same
cache. Return type changed from `tempfile::TempDir` (owned, cleaned up
per call) to a plain `PathBuf` (a persistent, `ociman prune`-managed
cache entry, not per-`COPY` scoped at all).

## Measured result

Real `docker.io/library/busybox:latest` (consumer) +
`docker.io/library/ubuntu:24.04` (external `--from=` source, pulled
once), both already pulled before timing starts.

**Three separate `COPY --from=` instructions referencing the same
external image in one Containerfile** (a real, if less common, pattern
— e.g. copying several distinct files out of a shared golden image):

| | before | after |
|---|---:|---:|
| mean | 834.2 ms | 101.8 ms |

**8.19× faster** — the second and third `COPY --from=ubuntu:24.04`
no longer pay any real extraction cost at all, since the first one
already populated the cache.

**A single `COPY --from=` (the common case, cache always cold — worst
case for this change, nothing to reuse)**:

| | before | after |
|---|---:|---:|
| mean | 377.6 ms | 338.6 ms |

Still **1.12× faster**, not slower, even with nothing to reuse:
`ensure_cached`'s own rename-into-place is marginally cheaper than the
old code's own "extract into a plain tempdir, then `Drop`-clean it up
at the end of the `copy_instruction` call" — the same real blob-reading
and layer-extraction work happens either way, just without the
create-then-immediately-throw-away tempdir wrapper around it.

## Disk-space safety re-checked, not assumed

Since the cache entry now *persists* after the build (instead of being
torn down the moment that one `COPY` finished), this looked like a
possible new disk-growth vector worth checking directly rather than
assuming away — this project's own repeated "ensure we don't run out
of disk space" standard. Traced through, not assumed: `resolve_or_pull`
(used by *both* the old and new code, unchanged by this increment)
already calls `store.put_image`, tagging the external image locally —
meaning the exact same manifest digest was already reachable via
`Store::list_images()` (and therefore already correctly retained by
`ociman prune`'s own rootfs-cache reachability check, 0111) even
*before* this change; the old code's own per-`COPY` extraction was pure
wasted work on top of an image that was going to stick around locally
regardless. Verified directly, not just reasoned about: built a real
image via three `COPY --from=ubuntu:24.04` instructions, confirmed
`ociman prune` correctly leaves the resulting cache entry alone (the
image is still tagged) — then `ociman rmi`'d the external image and
confirmed `ociman prune` then does reclaim it (removed 1 entry,
~100 MB). No new leak; the existing 0111 mechanism already covers this
case correctly, unchanged.

## Real, automated tests

No new tests needed — `copy_from_an_external_image_pulls_and_copies_a_
real_file` (already existing) exercises this exact code path
end-to-end and continues to pass unmodified, along with all 44 other
`ociman build` integration tests. `cargo test --workspace --locked`
clean, `cargo clippy --workspace --all-targets --locked -- -D
warnings` clean.
