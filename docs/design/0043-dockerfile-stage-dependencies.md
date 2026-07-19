# Design note 0043: resolving stage dependencies and target-stage pruning (milestone 4)

Status: implemented
Scope: `crates/oci-dockerfile/src/dependencies.rs`.

0041/0042 both flagged "dependency-ordered execution and target-stage
selection" as still-future work once stage grouping and expansion
landed. This increment resolves the *structural* half of that: which
stages depend on an earlier stage's own build output (rather than an
external image to pull), and which stages actually need to be built at
all for a given target — matching real `docker build --target`'s own
pruning optimization. Actual build *execution* (running the resolved
order, skipping pruned stages) is still separate, later work — this
increment only computes the graph.

## A deliberate, narrower scope than real BuildKit's own general graph

Real BuildKit's own dependency resolution (`dockerfile2llb/
validations.go`'s `validateCircularDependency`) is more general than
this increment: it builds a graph that could, in principle, contain a
forward reference (a stage's own base naming a stage declared *later*
in the file) and only rejects the result if that graph actually
contains a *cycle* — checked directly, not assumed, since it seemed
worth confirming rather than guessing given how unusual that would be
in practice.

This increment deliberately doesn't replicate that generality: a
stage's own `base_name` is only ever resolved against a stage
*earlier* in the same file — matching the overwhelmingly common
real-world multi-stage-build shape, and this crate's own stated target
(OS image customization Containerfiles, none of which reference a
stage that hasn't been declared yet). With this backward-only design,
a cycle is **structurally impossible to construct in the first
place** — a stage can only ever depend on something with a strictly
smaller index — so there is nothing to detect, and no separate cycle
check is needed at all: the dependency graph this increment produces
is trivially a DAG, and ordinary ascending file order already respects
every dependency edge in it.

## API

`resolve_dependencies(stages) -> Vec<Option<usize>>`: for each stage
(by index), `Some(earlier_index)` if its own `base_name` matches an
earlier stage's own name (case-insensitively, first match wins —
matching real `HasStage` exactly), or `None` if it's an external image
reference to pull instead.

`stages_needed_for(deps, target) -> Vec<usize>`: every stage index
that must be built to produce `target` — the target itself, plus every
stage it transitively depends on — in ascending order (always already
a valid build order here, per the "backward-only" reasoning above).
Stages that don't contribute to the requested target at all are
pruned entirely, matching real `docker build --target`'s own
optimization of not building work nothing downstream needs.

A `COPY --from=<stage>` reference is a *different* kind of cross-stage
dependency from a stage's own `FROM <stage>` base — this increment
deliberately only resolves the latter (a stage's *own* base image);
resolving `COPY --from=` references (which affect what needs to be
built for `stages_needed_for` to be fully correct, but require walking
every instruction in every stage rather than just each stage's own
`FROM` header) is left for the same future build-execution increment
that will actually need to act on both together.

## Real, automated tests

Six new unit tests: an external-image stage has no dependency; a
stage using an earlier named stage as its own base is correctly
resolved; a stage merely being *referenced* via `COPY --from=` (as
opposed to being *used as another stage's own base*) is correctly
*not* treated as a dependency by this increment's own narrower scope;
a stage naming something that only becomes a *later* stage is treated
as an external reference, not a forward dependency, confirming the
deliberate backward-only design; `stages_needed_for` correctly pruning
an unrelated stage; and `stages_needed_for` correctly including
transitive dependencies while excluding stages downstream of the
requested target.

## Performance

Not called from anywhere yet — no build-execution increment exists to
call it — so zero runtime impact on any hot path by construction, same
reasoning every earlier increment in this pipeline (0039-0042) used.

## What's still not here

* `COPY --from=<stage>` references aren't resolved into the dependency
  graph at all yet (see above) — only a stage's own `FROM <stage>`
  base is.
* No target-name-to-index resolution helper is provided — a future
  caller is expected to use the already-existing `find_stage` (0041)
  for a named `--target`, or default to the last stage's own index
  (real `docker build`'s own default when no `--target` is given),
  then pass that index to `stages_needed_for` directly.
* Actual build execution, the build cache, `ONBUILD`/`HEALTHCHECK`,
  heredocs, the BuildKit-only flags, and `--build-arg` — all still
  future work, exactly as 0039-0042 already scoped.
