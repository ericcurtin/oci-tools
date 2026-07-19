# Design note 0053: multi-stage `ociman build` — `FROM <stage>` (milestone 4)

Status: implemented (`FROM <earlier-stage-name>` only — `COPY
--from=<stage-or-image>` is still not supported, a separate later
increment)
Scope: `bin/ociman/src/build.rs`, `tests/tests/ociman_build.rs`.

0052's own "what's still not here" named this next: *"Multi-stage
builds ... are not yet implemented."* This increment ships the first
half of that: a later stage's own `FROM` can now reference an earlier
stage by name, using that stage's own already-built result directly.
`COPY --from=<stage>` (the *other* kind of cross-stage reference —
copying files, not basing an entire stage) is deliberately deferred to
its own increment; see "Why not both at once" below.

## Reusing 0043's own dependency graph, unchanged

`oci_dockerfile::resolve_dependencies`/`stages_needed_for` (0043) have
existed, fully tested, since before any of 0050-0052 landed —
`cmd_build` had simply never called them, rejecting every Containerfile
with more than one `FROM` outright. This increment is purely a
`bin/ociman` wiring change: **zero lines changed in `oci-dockerfile`
itself.** `resolve_dependencies` already only tracks `FROM <name>` as a
dependency (its own doc comment is explicit that `COPY --from=`
is a *different* kind of cross-stage reference it deliberately doesn't
resolve) — which turns out to line up exactly with this increment's
own scope, not by coincidence: the "`FROM`-only" restriction was
already baked into the crate this increment reuses, so implementing
`COPY --from=` too in the same pass would have meant *also* extending
`oci-dockerfile`'s own dependency graph and its own test suite, a
meaningfully bigger and riskier change than reusing what already
exists as-is.

## What actually changed in `cmd_build`

Previously: parse -> one stage -> resolve its external base -> build.
Now: parse -> every stage -> `resolve_dependencies` -> `stages_needed_
for(deps, target)` (target is always the *last* stage, matching real
`docker build`'s own default with no `--target`, which doesn't exist
as a flag yet) -> build every needed stage **in `stages_needed_for`'s
own ascending order** (always dependency-safe here, since this
project's own backward-references-only design makes the dependency
graph a trivial DAG — checked directly against 0043's own doc comment,
not re-derived).

Each built stage's own `ImageConfig`/layer list is kept in a
`HashMap<usize, BuiltStage>` for the rest of the build. When a later
stage's own `FROM` resolves to an earlier one (`deps[i] ==
Some(earlier)`), its starting `config`/`layers` are simply **cloned
from that earlier stage's own already-built result** — no store
lookup, no re-pulling, and critically, **no re-running anything**: a
dependency stage's own `RUN`/`COPY` steps already committed their own
real layers into the store the first (and only) time that stage was
built; a dependent stage just extracts those same already-stored
layers, exactly the same call (`oci_layer::apply` off `store.open_
blob`) it would use for an external image's own layers.

A stage nothing later depends on (an unrelated stage, or — until the
next increment — one referenced *only* via `COPY --from=`) is pruned
by `stages_needed_for` and never built at all, matching real `docker
build --target`'s own pruning behavior even without an explicit
`--target` flag existing yet.

## Why not both `FROM <stage>` and `COPY --from=<stage>` at once

Tempting to ship together (real multi-stage Dockerfiles overwhelmingly
use `COPY --from=` far more than `FROM <stage>` in practice), but
`COPY --from=` needs its own dependency-graph extension in `oci-
dockerfile` (today's `resolve_dependencies` explicitly, by design,
ignores it) — a change to a different, lower-level, already-stable
crate with its own test suite, not just a `bin/ociman` wiring change.
Keeping this increment to "reuse 0043 exactly as it already exists"
kept the change small and low-risk; `COPY --from=` is the natural next
increment once this one is proven solid.

## Real, manual end-to-end verification before writing automated tests

Built the release binary and ran a real two-stage build against a real
`docker.io/library/busybox:latest` pull: stage `builder` (`ENV FOO=bar`
+ a real `RUN` writing `/marker.txt`), then the target stage
(`FROM builder`, `ENV BAZ=qux` + a second `RUN` reading `builder`'s own
file). `ociman inspect`ed the result (exactly one real layer beyond the
base — `builder`'s own `RUN`, never re-committed a second time — plus
both `ENV` vars present) and, most convincingly, `ociman run` the
built image and confirmed both files existed with the right content
and both `ENV` vars were set, proving the whole cross-stage chain
(layers *and* config) survived intact.

## Real, automated tests

Replaces the now-obsolete `rejects_a_multi_stage_dockerfile_with_a_
clear_error` test (multi-stage is supported now) with two real tests:
the two-stage `FROM builder` scenario above (asserting exactly one new
layer beyond the base, both `ENV` vars present, and actually running
the built image to confirm real file content survives the chain), and
a pruning test that gives an *unreferenced* stage a `FROM` pointing at
an image that doesn't exist anywhere — proving it's never built at all
(if it were, the whole command would fail; instead the build succeeds
using only the real, referenced target stage).

## Performance

This increment touches only `bin/ociman/src/build.rs` and its own test
file — `oci-runtime-core`/`ocirun`/`ociman run`'s own hot paths are
untouched (confirmed via `git diff --stat`), so no benchmark
re-verification was needed. A single-stage build's own cost is
unchanged (`stages_needed_for` on a one-stage file just returns that
one stage, same as calling `build_stage` directly did before).

## What's still not here

* `COPY --from=<stage-or-image>` — the natural next increment, needing
  its own extension to `oci-dockerfile`'s dependency graph.
* `FROM scratch` (as any stage's base, not just a target's) — still
  rejected; only becomes useful once `COPY --from=` exists to put
  anything into an otherwise-empty rootfs.
* `--target`, `--build-arg`, the build cache, `ADD`, `ONBUILD`/
  `HEALTHCHECK`, an anonymous/untagged build mode — all still exactly
  as 0050-0052 left them.
