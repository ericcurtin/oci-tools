# Design note 0074: `ociman build` `COPY --from=<external-image>` (milestone 4)

Status: implemented
Scope: `bin/ociman/src/build.rs` (new `external_image_source_root`,
`copy_instruction`'s own `source_root` resolution extended to fall
through to it), `tests/tests/ociman_build.rs`.

`copy_instruction`'s own module doc comment has flagged `COPY
--from=<external-image>` (a `--from` name that isn't any earlier stage
in the same Containerfile) as "still not supported" since the very
first multi-stage increment (0053/0054) — the last item on `COPY`'s
own long-tracked scope list, now that 0072/0073 closed multiple
sources and glob patterns.

## No new machinery needed — reused exactly what `FROM <image>` already does

Real BuildKit's own `dispatchCopy`
(`~/git/moby/daemon/builder/dockerfile/dispatchers.go`) resolves
`--from` as a stage name first and, if that fails, treats it as an
ordinary image reference to pull. This project's own `cmd_build`
already has exactly the "parse a reference, `resolve_or_pull` it,
read its manifest, extract every layer" sequence for a stage's own
`FROM <image>` (not `FROM <earlier-stage>`) base — `external_image_
source_root` is that same sequence, verbatim, just writing the
extracted result into a fresh scratch directory instead of a stage's
own long-lived one, and returning a `tempfile::TempDir` the caller
keeps alive for the duration of the one `COPY` that needs it (the
same `_build_dir`-style pattern `BuiltStage` already established for
its own analogous case).

`stage_ctx.rootfs_for(from)` already correctly returns `None` for a
`--from` name that isn't a stage — `copy_instruction`'s own resolution
now just falls through to `external_image_source_root` in that case,
instead of the previous unconditional error. No change was needed to
`oci_dockerfile::resolve_copy_from_dependencies` at all: it already
only records a dependency edge when `--from` *does* match an earlier
stage (`.position(...)` returning `None` for anything else is filtered
out of the dependency list entirely), so an external-image `--from`
was already correctly invisible to the dependency graph/pruning logic
before this increment — confirmed by re-reading that function's own
existing implementation rather than assuming it needed a matching
change.

## Real, manual end-to-end verification before writing a single automated test

Built the debug binary and ran three real scenarios by hand: `COPY
--from=docker.io/library/busybox:latest /bin/busybox
/copied-busybox` against a real, already-pulled image — the resulting
file's own `md5sum` matched the original `/bin/busybox` byte for byte;
a build mixing both kinds of `--from` in the same Containerfile (one
`COPY --from=<earlier-stage>`, one `COPY --from=<external-image>`) —
both resolved correctly and independently; a `--from` naming a real
image that genuinely doesn't exist on the registry — failed with the
real registry's own `401`/`UNAUTHORIZED` error surfaced through the
same error-chain rendering every other registry failure in this
project already uses, not a special case.

## Real, automated tests — offline, matching this project's own established testing philosophy

`copy_from_an_external_image_pulls_and_copies_a_real_file` seeds the
"external" image into the *same isolated test store* ahead of time
(`seed_image_with_files`, already used elsewhere in this same test
file for unrelated purposes) so `resolve_or_pull` finds it already
present and the test never touches the real network — the same
established "exercised entirely offline" pattern every other test in
`ociman_build.rs`/`ociman_run.rs` already follows for `FROM`. One
existing test, `copy_from_rejects_a_name_that_is_not_any_earlier_stage`,
needed updating rather than just leaving alone: its own `--from=docker.io/
library/alpine:latest` case used to assert an unconditional rejection
("does not match any earlier stage") that would now either attempt a
real network pull (flaky, slow, and no longer testing what its own
name says) or succeed outright. Replaced with a genuinely-still-
rejected case — a `--from` value that's neither a real stage name nor
a syntactically valid image reference (`repository name must be
lowercase`) — keeping the test's own real intent (a name matching
neither category is rejected) while staying deterministic and
offline.

## Performance

This increment touches only `bin/ociman/src/build.rs`'s own `COPY`
instruction handling — not `oci-runtime-core`, `main.rs`'s
`synthesize_spec`/`resources_from_cli`, or either cgroup driver
(confirmed via `git diff --stat`), and none of this is on the `ociman
run`/`ocirun run` startup/destroy hot path this project's own
benchmarks measure. No benchmark re-verification was needed, consistent
with every prior build-only increment.

## What's still not here

* `ADD`'s own remote URL sources — the one remaining item on `ADD`'s
  own scope list (unlike `--from`, `ADD` has no `--from` at all, so
  this increment doesn't touch it).
* The build cache — still nothing actually caches a previous build's
  own result yet, unchanged by this increment.
* Caching a `--from=<external-image>` pull/extraction across multiple
  `COPY` instructions that happen to reference the *same* external
  image more than once in one build — each `COPY --from=<external-
  image>` still pulls and re-extracts independently; a real, if minor,
  optimization opportunity for a later increment, not pursued here to
  keep this one's own scope narrow and its own risk low.
