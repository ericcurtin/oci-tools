# Design note 0060: `ociman build`'s "unused build-args" warning (milestone 4)

Status: implemented
Scope: `crates/oci-dockerfile/src/stage.rs` (new `declared_arg_names`),
`bin/ociman/src/build.rs`, `tests/tests/ociman_build.rs`.

0059's own "what's still not here" named this exact follow-up:
*"the real 'one or more build-args were not consumed' warning ... a
real, well-scoped, smaller follow-up, deliberately not bundled into
this increment."*

## `declared_arg_names`: one small, reusable scan, in `oci-dockerfile` since it's about the Dockerfile's own structure

Checked directly against real buildah's own `imagebuildah/executor.go`:
"consumed" means "named by *any* `ARG` instruction anywhere in the
file" — every meta-`ARG` *and* every stage-local `ARG` in *every*
stage, including one `stages_needed_for` would prune as unreferenced
by the current build target. Real buildah's own token scan runs before
it even knows which stages the target actually needs, for exactly this
reason: an `ARG` declared only in an unrelated, pruned stage still
counts as a real declaration. `declared_arg_names(meta_args, stages) ->
HashSet<String>` lives in `oci-dockerfile::stage` (alongside
`group_stages`/`find_stage`, which already produce exactly the
`meta_args`/`stages` shapes it consumes) rather than in `ociman`
itself — it's a fact about the Dockerfile's own structure, independent
of any specific `--build-arg` values, and a natural, reusable sibling
to `find_stage`.

## The interesting logic is a plain, directly-testable function; printing is a thin wrapper

`unused_build_arg_names(meta_args, stages, build_args) -> Vec<&str>`
(`bin/ociman/src/build.rs`) is `declared_arg_names` filtered against
the resolved `--build-arg` overrides map's own keys, sorted for a
deterministic message. `warn_on_unused_build_args` is a thin,
one-line-body wrapper that only adds the actual `eprintln!` — kept
separate specifically so the interesting part (which names are
flagged, and in what order) is unit-testable without capturing
`stderr`.

## To stderr, after a successful build, never mixed into `--json`'s own output

Checked directly: real dockerd's own `buildargs.go`'s
`WarnOnUnusedBuildArgs` and real buildah's own `executor.go` both print
this exact message shape (`"[Warning] one or more build-args %v were
not consumed"` / `"...one or more build args were not consumed: %v"`)
as a **warning**, after a build finishes successfully — never a hard
error that fails an otherwise-good build over one likely-typo'd flag.
`ociman build`'s own version matches: printed via `eprintln!` (never
mixed into `--json`'s own machine-readable stdout output), after
`store.put_image` has already tagged the result.

## Real, manual end-to-end verification before writing automated tests

Built the debug binary and ran two real builds against the same
Containerfile (one declared `ARG VERSION=1.0`): with `--build-arg
VERSION=2.0` alone, no warning printed; with `--build-arg
NEVER_DECLARED=xyz --build-arg VERSION=3.0` together, the warning
printed listing *only* `NEVER_DECLARED` — confirming the consumed
`VERSION` override is correctly excluded, not lumped in just because
some other flag on the same invocation was unused.

## Real, automated tests

3 new unit tests in `oci-dockerfile::stage` for `declared_arg_names`
itself (collects meta-args and every stage-local `ARG`; includes a
name declared only in a stage pruned from the current target; empty
when nothing declares anything). 3 new unit tests in `bin/ociman` for
`unused_build_arg_names` (empty when every override matches a declared
name; flags an undeclared name, sorted deterministically when there
are several; does not flag a name declared only in a pruned stage —
mirroring the real buildah behavior above). 2 new integration tests in
`tests/tests/ociman_build.rs`: the real warning message appears on
`stderr` for a genuinely unused override (and a consumed sibling
override is correctly *not* listed), with the build still tagging the
image successfully; and no warning at all when every `--build-arg` is
consumed.

## Performance

This increment touches only `oci-dockerfile::stage` and `bin/ociman/
src/build.rs` — `oci-runtime-core`/`ocirun`/`ociman run`'s own hot
paths are untouched (confirmed via `git diff --stat`), so no benchmark
re-verification was needed. The scan itself is a single pass over
already-in-memory `meta_args`/`stages` data structures the build
already holds, run once per build after it completes.

## What's still not here

* `ADD`, `COPY --from=<external-image>`, the build cache, `ONBUILD`/
  `HEALTHCHECK`, an anonymous/untagged build mode, `--target` — all
  still exactly as 0050-0059 left them.
* Milestone 3's own remaining gaps (`--cap-add`/`--cap-drop`,
  `createContainer`/`startContainer` hooks, automated failed-systemd-
  scope cleanup, `--privileged`) — untouched by this increment.
