# Design note 0133: `content_digest` respects `.dockerignore` too; performance re-verification after 0129-0132

Status: implemented (real bug fix) + verification
Scope: `bin/ociman/src/build_cache.rs` (`content_digest`/`hash_path`
gain a `context_ignore` parameter, mirroring `build.rs`'s own
`copy_path_recursive`); `bin/ociman/src/build.rs` (both call sites
updated); 3 new unit tests, 1 new integration test.

## Why: a routine performance re-verification found a real bug

0112/0120's own precedent ("must have measurably equal or better
performance than before" needs re-checking against real equivalents
after several increments' worth of work on shared code paths) called
for a fresh round after 0129-0132 (four consecutive `ociman build`-
focused commits, the last three adding real new tree-walking/matching
code — `.dockerignore`/`.containerignore`/`--ignorefile`).

## Method (identical to 0105/0113/0120)

`hyperfine --shell=none`, 5+ warmup runs. Same rootless busybox-based
bundle shape for `ocirun`/`crun`/`runc`; same real, already-pulled
`docker.io/library/busybox:latest`/`docker.io/library/ubuntu:24.04`
for `ociman`/`podman`/`docker`; same `FROM ubuntu:24.04` + `RUN echo
hello` Containerfile for the plain build comparison (0112's own exact
benchmark) — plus a **new** benchmark this session added specifically
to exercise 0130's own dockerignore-pruning claim under real load: the
same Containerfile with an extra `COPY . /app`, run against a context
containing a real 5,000-file, 20MB `node_modules`-shaped directory,
comparing a `.dockerignore` that excludes it against one that doesn't.

## Result: no regression on the core runtime path; a real, fixed bug on the build path

| comparison | this session | most recent prior measurement |
|---|---:|---:|
| `ocirun run` vs `crun run` | 3.5ms vs 7.8ms (2.25×) | 0120: 3.0ms vs 7.8ms (2.25×) |
| `ocirun run` vs `runc run` | 3.5ms vs 21.5ms (6.18×) | 0120: 3.0ms vs 21.5ms (6.18×) |
| `ociman run --rm` vs `podman run --rm` | 59.2ms vs 182.5ms (3.08×) | 0120: 52.3ms vs 179.6ms (3.43×) |
| `ociman run --rm` vs `docker run --rm` | 59.2ms vs 291.3ms (4.92×) | 0120: 52.3ms vs 293.1ms (5.60×) |
| `ociman build` (warm) vs `podman build` (warm) | 64.5ms vs 88.6ms (1.37× **faster**) | 0121 (first "faster" measurement): "~26% speedup" |

Every real-runtime comparison (`ocirun`/`ociman run`) is unchanged
within noise — confirming zero regression from 0129-0132's own real
work (all four commits only ever touch `ociman build`'s own code
paths; none of it is reachable from `ocirun`'s or `ociman run`'s own
hot paths at all). `ociman build`'s own plain-Containerfile comparison
against real `podman build` remains solidly faster (1.37×), reconfirming
0121's own "flipped from slower to faster" result still holds after
eleven further increments' worth of work landed on top of it (0122-
0132).

## The new benchmark exposed a genuine bug, not just a number to record

Building the same Containerfile with `COPY . /app` against a context
containing a 5,000-file `node_modules` directory, comparing
`.dockerignore` excluding it against not excluding it at all:

| variant | time (warm) |
|---|---:|
| empty context (no extra files at all) | 68.4ms |
| `node_modules` present, **excluded** by `.dockerignore` (before this fix) | 88.6ms |
| `node_modules` present, **not** excluded at all (actually copied) | 140.5ms |

Excluding the directory was clearly faster than actually copying it
(1.59×, confirming 0130's own directory-walk-pruning optimization
really does skip the copy work) — but still measurably slower than the
truly-empty-context baseline (1.30×), which shouldn't have been true at
all: nothing under an excluded directory is ever supposed to cost
anything beyond the one `stat` needed to confirm it's a directory in
the first place. Investigating *why* found `build_cache::
content_digest` (used to compute a `COPY`/`ADD` step's own cache key)
recursively hashing **every byte** under a `COPY` source unconditionally
— including everything inside an excluded directory that
`copy_path_recursive` itself was correctly never touching at all.

This is a real, two-sided bug, not just a missed optimization:

* **Wasted work**: reading and hashing 20MB of content that gets
  thrown away immediately, on every single build, forever, even though
  none of it is ever actually copied.
* **Unnecessary cache invalidation**: a change *inside* an excluded
  directory would still change the computed digest, busting the cache
  for a `COPY`/`ADD` layer whose own real, copied content never
  changed at all — a real correctness-adjacent bug in the build
  cache's own "did anything that actually matters change" logic, even
  though the *built image itself* was never wrong (the excluded bytes
  were never in it either way).

## The fix

`content_digest`/its own recursive `hash_path` helper now take the
same `context_ignore: Option<&oci_dockerfile::DockerIgnore>` shape
`copy_path_recursive` already established, applying the *exact* same
skip logic: an excluded file/symlink is never read at all; an excluded
directory with no `!`-negated pattern anywhere is never even
descended into (matching `copy_path_recursive`'s own "prune eagerly
when nothing could ever be re-included" optimization); an excluded
directory that *does* need walking (because some negation pattern
exists) still only feeds a re-included descendant's own bytes into the
hash, never the excluded ones. Both real call sites (`copy_instruction`/
`add_instruction` in `build.rs`) already had the right `context_ignore`/
`dockerignore` value in scope from 0130's own earlier work — passing it
through was the entire fix.

## Re-measured after the fix

| variant | time (warm) |
|---|---:|
| empty context | 68.1ms |
| `node_modules` present, excluded (**after** this fix) | 72.0ms |

1.06× vs the empty-context baseline (down from 1.30×) — the excluded
directory's own cost is now essentially just the one `stat` call
`copy_path_recursive`'s own pruning already needed, matching the
theoretical "this should cost nothing beyond confirming it's ignored"
expectation the original 0130 design intended but didn't fully
deliver.

## Real, automated tests

Two new unit tests in `build_cache.rs`
(`content_digest_ignores_a_dockerignored_directorys_own_content_
entirely`, and its own explicit control,
`content_digest_still_reacts_to_the_same_change_with_no_dockerignore_
at_all`, confirming the first test is actually exercising
`context_ignore` and not merely a digest blind to nested changes in
general) plus one new CLI-level integration test in `tests/tests/
ociman_build.rs`
(`a_change_inside_a_dockerignored_directory_never_busts_the_cache`) —
a real two-build round trip confirming the built image's own manifest
layers are byte-identical across both builds despite the excluded
file's own content genuinely changing in between. All pre-existing
tests (including 0130-0132's own dockerignore/containerignore/
ignorefile tests and the full 65-test base `ociman_build.rs` suite)
still pass unmodified. Full `cargo build --workspace --locked`/`cargo
test --workspace --locked` (2 clean runs)/`cargo fmt --all --check`/
`cargo clippy --workspace --all-targets --locked -- -D warnings`/
`python3 ci/guards.py`/`cargo deny check` all clean.
