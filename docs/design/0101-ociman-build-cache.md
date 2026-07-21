# Design note 0101: `ociman build` local build cache (milestone 4)

Status: implemented
Scope: new `bin/ociman/src/build_cache.rs`; `bin/ociman/src/build.rs`
(`cmd_build`/`build_stage`/`apply_instruction`/`run_instruction`/
`copy_instruction`/`add_instruction` all thread a `&[CacheCandidate]`
through; new `reuse_cached_layer`/`copy_add_command_text`/
`ensure_sources_exist`); `bin/ociman/src/main.rs` (`--no-cache`);
`tests/tests/ociman_build.rs`.

Every design note from 0050 onward has flagged the same gap: "the
build cache — still nothing actually caches a previous build's own
result yet" (most recently `crates/oci-dockerfile/src/lib.rs`'s own
module doc and the README's milestone table). This closes it.

## Why buildah's model, not BuildKit's

`~/git/moby`'s current builder is BuildKit: a general content-
addressed DAG of LLB ops (`solver/cachekey.go`), a poor architectural
fit here since `ociman build` has no op graph at all — just
`build_stage`'s own straight-line loop over a real, live rootfs.
`~/git/podman`'s vendored `go.podman.io/buildah/imagebuildah/
stage_executor.go` is the same shape this project already has (a
plain sequential per-instruction executor over a real container
filesystem), so its model is what `build_cache.rs` ports directly onto
this project's own already-existing `ImageConfig.history`/
`rootfs.diff_ids` shape (`crates/oci-dockerfile/src/commit.rs`) —
no new on-disk metadata format needed at all.

The port, concretely:

* **Candidates are every image already in local storage**
  (`load_candidates`, one `store.list_images()` plus one
  `image_manifest`/`image_config` read each — done once per `ociman
  build` invocation, not once per instruction, since nothing about
  local storage changes mid-build). This is real buildah's own
  `intermediateImageExists`, which likewise scans every image in
  local storage rather than a separate cache index.
* **A cache hit** (`find_cached_layer`) needs a candidate whose own
  history has a strictly longer, entry-for-entry-identical prefix than
  the current build's progress so far (`created_by`/`empty_layer`
  only — not `created`/`author`/`comment`, which for the *prefix*
  portion are guaranteed equal by construction: both builds' prefixes
  ultimately come from the exact same, already-immutable base image),
  plus a next entry whose `created_by` matches exactly what the
  pending instruction would record — real buildah's own
  `historyAndDiffIDsMatch`, a full-history-prefix match, not a
  per-instruction lookup independent of position (so one miss really
  does invalidate every later step's own chance to hit, matching real
  Docker/BuildKit's well-known cache semantics).
* **A hit reuses the layer verbatim** (`reuse_cached_layer`): extracts
  the already-stored, already-compressed blob straight onto the live
  rootfs (`oci_layer::apply`) and splices its descriptor/diff_id/
  history entry into the build in progress — skipping the instruction
  entirely, not just skipping redundant I/O. For `RUN` in particular
  that means skipping a real namespace/rootless-uid-mapping/seccomp
  container launch outright, which is the whole point given this
  project's own top-level goal (beat real equivalents on startup/
  destroy time especially): a cache hit costs one `apply()` call, not
  one more `fork`+`unshare`+`exec`+`wait4` cycle.
* **The reused history entry keeps its own original `created`
  timestamp** rather than being stamped "now" — nothing new was
  actually produced at this build's own wall-clock time, so there's no
  truer timestamp to record. (Verified by an automated test: rebuild
  twice, assert the two configs' `history` lists are fully
  identical, not just their `layers`.)

## `RUN` needs no extra signature; `COPY`/`ADD` need a real content digest

`RUN`'s own cache key is exactly its already-recorded `created_by`
(the resolved command text — already reflects any `--build-arg`/`ARG`/
`ENV` substitution actually used inside the command line, since
`oci_dockerfile::expand_stage`'s own `$VAR` expansion runs before
`build.rs` ever sees it) — matching real Docker's own classic builder,
which busts a `RUN` layer's cache on command-text-plus-parent-chain
alone, with no filesystem content to hash in the first place.

`COPY`/`ADD` are different: their own `created_by` text (source/dest
names, `--from`) says nothing about whether the copied *bytes*
changed. `content_digest` closes that gap the same way real Docker's
own classic builder does — folding a real content digest of the
copied source tree directly into the recorded `created_by` string
itself (real Docker's own `docker history` shows exactly this
convention: `COPY dir:1414d0f7... in /app`) rather than inventing a
separate, unpersisted side channel a later build has no way to
recompute against. Computed by a small recursive hasher
(`hash_path`) over each resolved source's own relative path, type,
content (regular files) or link target (symlinks) — permission bits
deliberately excluded (`--chmod`/`--chown`, folded into `created_by`
separately via `copy_add_command_text`, already cover those). This
must run *before* the cache lookup (you cannot know whether copied
content changed without reading it — the same real, unavoidable
ordering real buildah's own `ContentDigester` has), so
`ensure_sources_exist` was added to check source existence *before*
hashing, keeping the existing "source does not exist" error message
intact instead of leaking a raw I/O error out of the hasher.

`ADD` with a remote URL source deliberately never attempts a cache
lookup at all (`add_instruction`'s own `url_sources.is_empty()` guard)
— fetching the URL just to hash it would defeat the entire point of a
hit, and this project doesn't implement real BuildKit's own `ETag`/
`Last-Modified`-based remote-content change detection that would make
a URL source cacheable without refetching it.

## `--no-cache`

Matches real `docker build --no-cache`/`podman build --no-cache`
exactly: `cmd_build` simply loads an empty candidate list instead of
calling `load_candidates`, so every `RUN`/`COPY`/`ADD` always misses
and re-executes for real.

## What's deliberately still not here

* No `--cache-from`/`--cache-to` remote cache import/export (real
  buildah's own `generateCacheKey` for exactly that) — local-storage-
  only, matching this project's own established narrow-first-increment
  pattern.
* No separate cache pruning — an image no longer in local storage
  (`ociman rmi`, not implemented yet either) simply stops being a
  candidate the next time `load_candidates` runs.

## Real, automated tests — the hard part was proving a hit is real

Digest equality alone doesn't prove a cache hit happened: two
genuinely *separate* real executions of a `RUN` step can coincidentally
produce byte-identical layers anyway (checked directly, the hard way,
while writing these tests — `echo $$ > /marker` is *not* a usable probe:
a freshly launched, single-process container's own shell is almost
always PID `1` in its own fresh pid namespace regardless of how many
times it's really launched, and two builds finishing within the same
wall-clock second share the same whole-second mtime too, so a command
with no other source of real entropy produces the exact same tar bytes
whether or not it actually re-ran). `/proc/sys/kernel/random/uuid` — a
real kernel interface that returns a genuinely fresh random value on
every single read, independent of PID namespace or mtime granularity —
is what the tests actually use as the "did this really execute again"
probe:

* `rebuilding_the_same_containerfile_reuses_previously_built_layers`:
  two builds of an identical Containerfile (`RUN cat /proc/sys/
  kernel/random/uuid > /marker.txt` then `COPY`) produce byte-for-byte
  identical manifests *and* fully identical `history` (including
  timestamps) — impossible unless the second build's `RUN` never
  really ran again.
* `no_cache_forces_a_real_re_execution_instead_of_reusing_a_cached_layer`:
  same setup, `--no-cache` on the second build — the two builds'
  layers now genuinely differ, proving the *default* (no flag) case
  above isn't just coincidental convergence.
* `a_copy_source_whose_content_changed_is_not_served_from_the_cache`:
  identical `COPY` instruction text, genuinely different source file
  content between builds — the cache must miss (a real content digest
  is what makes this distinction, not the instruction text alone) —
  and the built image really contains the new content, not a stale
  cached copy.
* `a_change_earlier_in_the_file_busts_the_cache_for_every_later_step_too`:
  a different `RUN` before an otherwise-unchanged `COPY` — the changed
  `RUN`'s own layer is asserted to differ (a real, fresh execution, not
  served from any candidate). The unchanged `COPY` after it is
  deliberately *not* asserted either way: copying the exact same file
  always produces byte-identical tar bytes regardless of whether real
  copying happened again or a cache lookup skipped it, in *any*
  per-instruction-diff architecture (this project's own included) —
  not a useful signal for "was the cache consulted here" the way a
  `RUN` step's own random-UUID probe is.
* `build_cache`'s own unit tests (`history_prefix_matches`/
  `find_cached_layer`/`content_digest`/`load_candidates`) cover the
  matching logic directly, without spawning a real container at all.

## Performance

This is the performance-motivated increment the top-level README's own
goal (beat every real equivalent on startup/destroy time especially)
calls for: a cache hit for `RUN` now costs one `apply()` extraction
instead of one more real rootless namespace launch (`fork`+`unshare`+
seccomp-filtered `exec`+`wait4`) — for a multi-`RUN` Containerfile
rebuilt with only a late change, every unaffected earlier `RUN` no
longer pays that cost at all, which is a strictly larger win than any
constant-factor optimization inside the launch path itself could give
for the same rebuild.
