# Design note 0054: `COPY --from=<stage>` (milestone 4)

Status: implemented (`--from=<earlier-stage-name>` only —
`--from=<external-image>` still rejected)
Scope: `crates/oci-dockerfile/src/dependencies.rs` (new function,
extended `stages_needed_for` signature), `bin/ociman/src/build.rs`,
`tests/tests/ociman_build.rs`.

0053's own "why not both at once" explained exactly what this
increment ships: `COPY --from=<stage>` — the *other* kind of
cross-stage reference, and by far the most common real-world
multi-stage pattern (build an artifact in one stage, copy just that
artifact into a fresh, minimal final stage).

## Extending `oci-dockerfile`'s dependency graph, not duplicating it

`resolve_dependencies` (0043) only ever tracks a stage's own `FROM
<name>` — by its own design, explicitly not `COPY --from=`. Rather than
overload that function's return shape (`Vec<Option<usize>>`, "at most
one dependency"), which can't represent "a stage's own `COPY`
instructions may reference several different earlier stages, none of
them its own base," this increment adds a **separate** function:

```rust
pub fn resolve_copy_from_dependencies(stages: &[Stage]) -> Vec<Vec<usize>>
```

One entry per stage, listing every earlier stage its own `COPY
--from=` instructions reference by name (case-insensitively, matching
real `HasStage`) — `--from=<external-image>` (not matching any stage
name) contributes nothing, matching real `parseCopy`'s own "stage names
take precedence" resolution order. `stages_needed_for` (0043) is
extended to take this alongside the existing `deps`, combining both
into one transitive-closure walk — a small, mechanical, backward-
compatible-in-spirit change (every existing caller just needs to pass
the new parameter; the underlying algorithm is unchanged, just walking
two edge lists per node instead of one).

## What actually changed in `ociman build`

Each `BuiltStage` now also keeps its own materialized rootfs directory
alive (a `PathBuf` plus the owning `tempfile::TempDir`, both held for
as long as the `BuiltStage` itself lives in `cmd_build`'s own `built`
map) — the piece `COPY --from=<stage>` actually reads from. A stage
`build_stage` wouldn't otherwise have materialized a rootfs for (no
`RUN`/`COPY` of its own) is now forced to anyway, via a new
`force_rootfs` flag, whenever `cmd_build`'s own upfront scan finds it
listed in *any* other stage's `resolve_copy_from_dependencies` entry —
otherwise there would be nothing on disk for that later `COPY` to read.

`copy_instruction`'s own source-root resolution now branches on
`flags.from`: `None` still means the build context (0052, unchanged);
`Some(name)` resolves `name` against every stage built so far (a new
`StageContext` wrapper bundling the stage list and the `built` map) and
copies out of *that* stage's own rootfs directory instead — the exact
same `safe_join`/`copy_path_recursive` logic either way, just a
different starting root. A name that doesn't match any earlier stage
is still rejected with a clear error (copying from an arbitrary
external image needs pulling and extracting it, still out of scope).

## The classic pattern now genuinely works

`COPY --from=<stage>` intentionally discards everything about the
source stage *except* whatever files were explicitly copied out — a
final stage's own manifest only ever gains the layers *it itself*
produces (its own base, plus its own `RUN`/`COPY` steps); a dependency
stage's own separate layers (e.g. `builder`'s own `RUN` output) never
become part of the final image's own layer list at all. Verified
directly, not assumed: a two-stage build (`builder`: one `RUN`
producing `/app.bin`; final: a *fresh* copy of the same base image,
`COPY --from=builder /app.bin ...`) produces a final image with
exactly one layer beyond its own base — and running it confirms
`/app.bin` itself (only ever present in `builder`'s own separate
rootfs) never leaks into the final image at all, only the file it was
explicitly copied to.

## Real, manual end-to-end verification before writing automated tests

Built the release binary and ran the exact scenario above against a
real `docker.io/library/busybox:latest` pull, confirming the artifact
survives the cross-stage copy with the right content, and that
`builder`'s own top-level file doesn't leak into the final image.
Separately verified: `COPY --from=<external-image>` is still rejected
with a clear error; and a three-stage Containerfile where one stage is
referenced only via `COPY --from=` and a second, wholly unrelated stage
(pointed at an image that doesn't exist anywhere) is still pruned and
never built — both kinds of cross-stage reference feed the same
pruning logic correctly, together.

## Real, automated tests

4 new tests, plus 2 in `oci-dockerfile`'s own `dependencies.rs`
(`resolve_copy_from_dependencies_tracks_stage_references_but_not_
external_images`, `stages_needed_for_includes_a_copy_from_dependency_
and_its_own_transitive_base`) and one existing test updated to reflect
the new (correct) combined pruning behavior. In `ociman_build.rs`: the
classic build-then-copy-artifact scenario (asserting exactly one new
layer and that the source stage's own other files don't leak in, then
actually running the built image); rejecting a name that isn't any
earlier stage; and a stage referenced only via `COPY --from=` being
built while a wholly unrelated third stage stays pruned.

## Performance

This increment touches only `oci-dockerfile`'s own dependency-graph
module and `bin/ociman/src/build.rs` — `oci-runtime-core`/`ocirun`/
`ociman run`'s own hot paths are untouched (confirmed via `git diff
--stat`), so no benchmark re-verification was needed. A build with no
`COPY --from=` at all pays nothing extra: `resolve_copy_from_
dependencies` on a file with no such instructions returns all-empty
lists, and `force_rootfs` is `false` for every stage.

## What's still not here

* `COPY --from=<external-image>` (pulling and extracting an arbitrary
  other image just for a `COPY`).
* `ADD`, `--target`, `--build-arg`, the build cache, `ONBUILD`/
  `HEALTHCHECK`, an anonymous/untagged build mode, `FROM scratch` (as
  any stage's base) — all still exactly as 0050-0053 left them.
