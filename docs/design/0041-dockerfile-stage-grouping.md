# Design note 0041: grouping instructions into build stages (milestone 4)

Status: implemented
Scope: `crates/oci-dockerfile/src/stage.rs`.

0039 shipped the flat instruction parser; 0040 shipped the `$VAR`/
`${VAR}` expansion engine, deliberately not wired into instruction
dispatch yet because that needs to know each instruction's own
accumulated `ARG`/`ENV` environment — which resets at every `FROM`.
This increment builds the piece both of those were waiting on:
grouping a flat instruction list into per-stage groups by their own
`FROM` boundaries, the prerequisite the crate's own module doc has
called out since 0039.

## Grounded directly against the real implementation

Checked directly against BuildKit's own `instructions.Parse`
(`~/git/moby/vendor/github.com/moby/buildkit/frontend/dockerfile/
instructions/parse.go`), not re-derived from documentation:

* **Before the first `FROM`, only `ARG` is legal.** Anything else has
  no stage to belong to at all — real error text, reused here
  verbatim: `"no build stage in current context"` (from real
  `CurrentStage`).
* **Stage names are not required to be unique.** A later `FROM ... AS
  name` reusing an earlier stage's own name isn't rejected at parse
  time — real `HasStage` just returns the *first* match by
  case-insensitive comparison; which stage a duplicate name actually
  resolves to during a real build is a later, dispatch-time concern,
  not something this increment invents stricter behavior for.
* Stage-name lookup (`find_stage`, mirroring real `HasStage`) is
  case-insensitive — even though this crate's own parser (0039) already
  lower-cases every stage name as it's declared (matching real
  `parseBuildStageName`), `find_stage`'s own comparison doesn't rely on
  that alone; it does its own case-insensitive comparison too, in case
  a future caller ever constructs a `Stage` some other way.

## API

`group_stages(instructions: Vec<Instruction>) -> Result<(Vec<Instruction>, Vec<Stage>), String>`:
walks the flat list once, collecting leading `ARG`s into a returned
meta-args list and appending every other instruction to whichever
`Stage` is currently open (the most recently seen `FROM`), erroring
immediately if a non-`ARG` instruction appears before any `FROM` at
all.

`Stage` holds the `FROM`'s own `base_name`/`name`/`platform` (copied
straight out of the `Instruction::From` that started it — deliberately
*not* re-parsing or duplicating that logic) plus every instruction that
followed, up to (not including) the next `FROM`.

`find_stage(stages, name) -> Option<usize>`: the building block a
later dependency-resolution increment will need to turn a `FROM
builder` (referencing an earlier stage by name) into an actual
dependency edge — not wired into anything yet, since nothing computes
a dependency graph at all so far.

## Real, automated tests

Six new unit tests: a single stage, meta-`ARG`s collected correctly
before the first `FROM`, the exact real error message for anything
else appearing before the first `FROM`, a real two-stage multi-stage
build (`FROM ... AS builder` / `FROM scratch AS final` / `COPY
--from=builder`) grouped correctly with each stage's own instructions
in the right place, and `find_stage`'s own case-insensitivity.

## Performance

Not called from anywhere yet (`expand`/`group_stages` are both still
independent, unwired primitives — see "What's still not here" below),
so zero runtime impact on any hot path by construction, same reasoning
0039/0040's own "Performance" sections used.

## What's still not here

* **Not yet combined with `expand` (0040).** Walking each stage's own
  instructions in order, threading an accumulating `ARG`/`ENV`
  environment through, and calling `expand` on exactly the instruction
  fields real BuildKit actually expands (and *not* on `RUN`/`CMD`/
  `ENTRYPOINT`/`SHELL`'s own command-line text — see 0040's own closing
  note for why that distinction matters) is the natural next increment
  that finally combines both already-built primitives.
* `FROM builder` isn't resolved against `find_stage` anywhere yet —
  there's no dependency graph, no target-stage selection, and no
  actual build execution (`RUN` steps via `oci-runtime-core`, layer
  commits via `oci-store`) — all still future work, exactly as 0039's
  own module doc already scoped.
* No validation that a stage's own `base_name`, once it *does* get
  resolved, doesn't reference a *later* stage (a forward reference,
  which real Docker also rejects, effectively, since a later stage
  can't have been built yet) — deferred to whichever increment
  actually builds the dependency graph.
