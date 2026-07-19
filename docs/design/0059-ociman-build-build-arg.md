# Design note 0059: `ociman build --build-arg` (milestone 4)

Status: implemented (override support; the real "unconsumed build-arg"
warning is a separate, smaller follow-up — see "What's still not
here")
Scope: `crates/oci-dockerfile/src/expand_stage.rs` (new `overrides`
parameter on both `expand_meta_args` and `expand_stage`),
`crates/oci-dockerfile/src/lib.rs` (stale doc comment refresh),
`bin/ociman/src/build.rs`, `bin/ociman/src/main.rs`.

`oci-dockerfile`'s own top-level doc comment has flagged this exact
gap since 0042: *"`--build-arg` (an external override for a
meta-`ARG`'s own value) has no representation at all yet —
`expand_meta_args` only ever sees each `ARG`'s own inline default."*
With `ociman build`'s own RUN/COPY/multi-stage support now shipped
(0050-0054), this was the clearest remaining milestone-4 CLI gap.

## Parsing stays entirely in `ociman`; `oci-dockerfile` only ever sees an already-resolved map

Checked directly, not guessed: real `podman build --build-arg`'s own
CLI-argument parser (`~/git/podman`'s own vendored `buildah/pkg/cli/
build.go`'s `readBuildArg`) is a small, pure string-parsing function —
`KEY=value` uses `value` verbatim; a bare `KEY` (no `=`) pulls the
value from the calling process's own environment if set there, or is
dropped entirely (not an empty-string override) if it isn't. This has
nothing to do with Dockerfile parsing or expansion, so it lives
entirely in `ociman build`'s own new `parse_build_args` (`bin/ociman/
src/build.rs`) — `oci-dockerfile` itself only ever receives an
already-resolved `HashMap<String, String>`, keeping the crate's own
long-standing "no CLI concerns of its own" boundary intact.

## Two real, checked-directly rules for when an override actually applies

Checked independently against **three** separate real implementations
(classic dockerd's `buildargs.go`, BuildKit's `dockerfile2llb/
convert.go`, and buildah's `executor.go`), which all agree:

1. **An override only takes effect for an `ARG` name actually declared
   somewhere in the file** — a meta-`ARG` before the first `FROM`, or
   any stage-local `ARG` (with or without its own inline default).
   `expand_meta_args`/`expand_instruction`'s own `Arg` match arm both
   check `overrides.get(name)` and simply do nothing further if the
   name was never declared — no error, no injection, matching real
   dockerd's own `getBuildArg` exactly (`mapping[key]`-gated).
2. **An override wins outright and is used *verbatim*, never
   re-`$VAR`-expanded** — even over a stage-local `ARG FOO=bar`'s own
   inline default, not just a bare meta-arg redeclaration (real
   BuildKit's own `dispatchArg` checks `hasValue` *before*
   `hasDefault`). This is genuinely new behavior beyond what existed
   before this increment: the pre-existing "bare `ARG NAME`
   redeclaration pulls from `global_args`" pathway only ever helped a
   *bare* stage-local redeclaration; it did nothing for a stage-local
   `ARG` with its own inline default, which real `docker`/`podman`
   both still let `--build-arg` override.

Both `expand_meta_args` and `expand_stage`/`expand_instruction` gained
the same new `overrides: &HashMap<String, String>` parameter,
checked in the same order everywhere it's consulted: override first
(verbatim), then the instruction's own inline default (expanded), then
(meta-arg case only) nothing, or (stage-local bare case only) a
fallback to `global_args`.

## A stale crate-level doc comment, fixed alongside this increment

`oci-dockerfile`'s own top-level module doc still said, verbatim,
*"nothing here executes a build yet"* and *"nothing actually builds
anything yet"* — both false since 0050 shipped a real, working
`ociman build` months of increments ago; the doc simply hadn't been
touched since before that work started. Rewritten to describe the
crate's own actual, current, narrower role (parsing, expansion, the
dependency graph, and the one piece of layer-commit glue) now that the
build *executor* itself lives in `ociman build`, not this crate.

## Real, manual end-to-end verification before writing automated tests

Built the debug binary and ran a real build against a Containerfile
with two `ARG`s (one overridden, one not), confirming via `ociman
inspect` that the overridden `ARG`'s own `ENV` derivative picked up the
new value while the other kept its own original default. Separately
verified: a bare `--build-arg VERSION` (no `=`) correctly pulled the
value from `ociman`'s own process environment; and an override for a
name nothing in the file declares has no effect at all (the build
still succeeds, using the declared `ARG`'s own original default) —
exactly the real, checked-directly rule from real dockerd/BuildKit/
buildah above.

## Real, automated tests

5 new unit tests in `oci-dockerfile::expand_stage` (override replaces
a meta-arg's own inline default; an override for an undeclared name
has no effect; override replaces a *stage-local* inline default, not
just a bare redeclaration; override used verbatim, not re-expanded —
even when it contains text that looks like `$VAR` syntax; a meta-arg's
own override still flows through a bare stage-local redeclaration).
5 new unit tests in `bin/ociman` for `parse_build_args` itself
(`KEY=value` verbatim; bare `KEY` from the real process environment;
bare `KEY` absent from the environment is dropped, not an empty
string; later `--build-arg` entries for the same key win; several
independent keys). 2 new integration tests in `tests/tests/
ociman_build.rs`: the full override-plus-untouched-default scenario
(asserting real `ENV` values by actually running the built image), and
the undeclared-name-has-no-effect scenario.

## Performance

This increment touches only `oci-dockerfile`'s own parsing/expansion
module and `bin/ociman/src/build.rs`/`main.rs` — `oci-runtime-core`/
`ocirun`/`ociman run`'s own hot paths are untouched (confirmed via
`git diff --stat`), so no benchmark re-verification was needed. A
build with no `--build-arg` at all pays nothing extra: `parse_build_
args` on an empty slice returns an empty map, and both `oci-dockerfile`
functions behave identically to before once `overrides` is empty.

## What's still not here

* The real *"one or more build-args were not consumed"* warning real
  `docker build`/`podman build` both print for a `--build-arg` name
  nothing in the file ever declares — needs a small, separate pass
  collecting every `Instruction::Arg` name across `meta_args` and
  every stage's own `instructions`, diffed against the overrides map's
  own keys after the build finishes. A real, well-scoped, smaller
  follow-up, deliberately not bundled into this increment.
* `ADD`, `COPY --from=<external-image>`, the build cache, `ONBUILD`/
  `HEALTHCHECK`, an anonymous/untagged build mode, `--target` — all
  still exactly as 0050-0058 left them.
