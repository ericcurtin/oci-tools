# Design note 0051: `ociman build`'s `RUN` step executor (milestone 4)

Status: implemented (single-stage `RUN`, no build cache yet)
Scope: `bin/ociman/src/build.rs`; one real bug fix shared with
`ociman run` (`bin/ociman/src/main.rs::synthesize_spec`).

0050's own "what's still not here" named this exact next piece: *"a
`RUN`-step executor ... is the natural next piece, since every other
primitive it needs already exists."* This increment is that executor —
`RUN` now runs a real command in a real rootless container against the
build's own materializing rootfs, diffs what changed, and commits it
as a genuinely new stored layer, using nothing but primitives already
shipped (0045-0049) and already used by `ociman run`.

## The pipeline, one `RUN` at a time

A stage's own scratch rootfs is materialized **once**, lazily — only
if the stage actually contains a `RUN` at all (checked up front via a
single scan; a metadata-only Containerfile, 0050's whole surface,
still never pays for a tempdir or a base-layer extraction it doesn't
need) — by applying every one of the base image's own layers via
`oci_layer::apply`, exactly the same call `ociman run`'s own `cmd_run`
already makes. That rootfs then persists, cumulatively, across every
`RUN` in the same stage (a later `RUN` sees everything an earlier one
left behind — real Docker's own layering semantics), living inside a
single `tempfile::TempDir` that outlives the whole `cmd_build` call.

For each `RUN`, in order:

1. `run_step_spec` builds a minimal rootless `Spec` — the step's own
   argv (`args_for`, already used for `CMD`/`ENTRYPOINT`), and the
   working directory/environment/user **as of this point in the
   build** (read from `config`'s own `ContainerConfig`, already
   mutated by any earlier `WORKDIR`/`ENV`/`USER` instruction in the
   same stage — a `RUN` genuinely sees the environment the Dockerfile
   has built up so far, matching real Docker).
2. `oci_layer::Snapshot::capture(rootfs)` — the "before" state (0045).
3. `oci_runtime_core::launch::run(id, &bundle, &rootfs)` — the exact
   same namespace/rootless-uid-mapping/seccomp machinery `ocirun run`/
   `ociman run` already use (`launch::run` is `run_reporting_pid` with
   no log path, no cgroup bookkeeping, and no `on_pid` callback — the
   simplest already-existing entry point, a perfect fit for an
   ephemeral, synchronous build step with no persistent-container
   concept of its own).
4. A **nonzero exit aborts the whole build** (`anyhow::ensure!`) —
   unlike `ociman run`, which forwards a container's own exit code as
   its own (a container that intentionally exits nonzero is still a
   "successful run"), a failed `RUN` step is always a build failure,
   matching real `docker build`/`podman build`.
5. `oci_layer::changes(rootfs, &before)` — the "after" diff (0045).
6. `oci_dockerfile::commit_layer`/`record_layer` (0048/0049) — commits
   the diff as a real stored layer and folds it into the working
   `ImageConfig`/manifest layer list, exactly like every metadata
   instruction already does via `record_empty_history`, just with a
   real layer attached this time.

No new primitive was needed anywhere in this chain — every piece
already existed, individually tested, from a previous increment.

## A real, pre-existing bug, caught by manual verification before writing any test

Manually running the very first real `RUN` step against a real
`docker.io/library/busybox:latest` pull (before writing any automated
test, per this project's own established practice) failed outright:
`can't create /marker.txt: Read-only file system`. Tracing it down:
`oci_spec_types::runtime::Spec::example()`'s own `root.readonly`
defaults to `true` — a reasonable default for a hand-written *example*
spec, but `synthesize_spec` (`ociman run`'s own spec builder, shipped
since milestone 3) never overrode it. **This means every `ociman run`
container, not just a build step, has been completely unable to write
anywhere in its own rootfs since the feature first shipped** — never
caught before because no existing `ociman_run`/`ociman_ps` test
happened to write anything inside a container. Confirmed directly: the
exact same failure reproduces via a plain `ociman run ... -- /bin/sh -c
"echo hi > /marker.txt"` against the *already-released* binary, with
no `ociman build` code involved at all.

Fixed in `synthesize_spec` itself (`main.rs`) — not just in this
increment's own new `run_step_spec` — since real `docker run`/`podman
run` both give a container a writable rootfs by default (only
`--read-only`, which neither surface exposes as a flag yet, makes it
read-only); leaving `ociman run` silently broken while fixing only the
new build code would have been a strictly worse outcome than not
noticing the bug at all. This is also a genuine, if small, performance
win, not merely a correctness fix: `oci_runtime_core::rootfs`'s own
bind-then-remount-readonly step (checked directly,
`non_readonly_root_skips_the_remount`) is skipped entirely once
`readonly` is `false` — one fewer mount syscall pair per container
start, for both `ociman run` and every `RUN` build step now.

## Real, benchmark re-verification (this touches `ociman run`'s own hot path)

`synthesize_spec` is exactly the kind of shared, already-benchmarked
code this project's own discipline requires re-verifying before/after.
Direct git-stash A/B hyperfine comparison, real `ociman run --rm
docker.io/library/busybox:latest -- /bin/true`, 30 runs each: before
50.8ms mean, after 47.4ms mean — the fix measures as a small real
improvement (matching the "one fewer mount syscall" expectation
above), not a regression, well within this project's own established
noise tolerance either way. `ocirun`/`oci-runtime-core` themselves are
untouched by this increment (confirmed via `git diff --stat` before
benchmarking — only `bin/ociman/*` changed), so `ocirun run`'s own
separately-tracked baseline needed no re-verification.

## Real, manual end-to-end verification before writing automated tests

Built the release binary and ran a real multi-`RUN` build against a
real `docker.io/library/busybox:latest` pull: two `RUN` steps (one
writing a top-level file, one creating a directory and a file inside
it) followed by an `ENV`, then `ociman inspect`ed the result (two new
layers, one non-empty-layer history entry per `RUN`, correct
`created_by` text) and, most convincingly, `ociman run`the *built*
image and `cat`'d both files back out for real, confirming the
content survived the whole diff/export/compress/ingest/manifest-update
round trip. Also verified a failing `RUN` (`RUN false`) aborts the
build with a clear error and leaves nothing tagged.

## Real, automated tests

`tests/tests/ociman_build.rs`'s own former
`rejects_a_run_instruction_with_a_clear_error` test (now obsolete —
`RUN` is supported) is replaced by two real tests: the full two-`RUN`-
plus-`ENV` scenario above (asserting the manifest's own layer count,
`rootfs.diff_ids` count, per-instruction history entries and their
`empty_layer` flags, *and* actually running the built image to confirm
real file content survives end to end — not just that new layer blobs
exist), and a failing-`RUN` test confirming the build aborts and
leaves nothing tagged (mirroring every other rejection path's own "no
partial image" contract already established in this file).

## Performance

`RUN` support adds real work (fork/exec/namespace setup per step) only
when a Containerfile actually contains a `RUN` — a metadata-only build
(0050's whole surface) still never materializes a rootfs at all, so
its own cost is unchanged. `ociman run`'s own hot path measures
slightly *faster*, not slower, per the re-verification above.

## What's still not here

* `COPY`/`ADD` (real build-context file access — the next natural
  increment, needing no container execution at all, only tar/copy
  logic).
* Multi-stage builds, `--build-arg`, the build cache, `ONBUILD`/
  `HEALTHCHECK`, an anonymous/untagged build mode — all still exactly
  as 0050 left them.
* `RUN --mount=` and other BuildKit-only flags (already rejected at
  parse time, unchanged).
