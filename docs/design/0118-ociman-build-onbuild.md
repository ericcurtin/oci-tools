# Design note 0118: `ociman build` supports `ONBUILD`, real cross-build execution included

Status: implemented
Scope: `crates/oci-dockerfile/src/instruction.rs` (`Instruction::
Onbuild`, `parse_onbuild`, `parse_onbuild_trigger`); `crates/oci-
dockerfile/src/expand_stage.rs` (one more "deliberately untouched"
match arm); `crates/oci-dockerfile/src/lib.rs` (exports `parse_onbuild_
trigger`); `crates/oci-spec-types/src/image.rs` (`ContainerConfig::
on_build`, plus a real bug fix: `null_as_default` needed for both this
new field and `HealthcheckConfig::test`); `bin/ociman/src/build.rs`
(`apply_instruction`'s own new match arm, `cmd_build`'s own per-stage
trigger-firing logic).

## Why this one is different from `HEALTHCHECK`'s own "parse and store, don't execute" scope

0116 deliberately limited `HEALTHCHECK` to parsing and storing —
actually running a healthcheck periodically is a separate runtime
concern, genuinely orthogonal to `build` itself (real `docker build`
doesn't execute one either; only a separate runtime monitoring loop
does). `ONBUILD` is not analogous: its entire purpose *is* to affect a
build — just a *different*, later one. A "parse and store, never
actually fire the trigger" implementation would not be a real subset
of `ONBUILD`'s behavior the way `HEALTHCHECK`'s scope-limit is a real
subset of *its* behavior — it would silently produce a **different,
wrong image** for any real Dockerfile relying on `ONBUILD` actually
doing something once used as a later build's own base (real, existing
official images have used this, e.g. historical `golang:onbuild`/
`node:onbuild` variants) — a worse outcome than the previous clear
parse-time rejection. This increment therefore implements the *real*,
full, cross-build-triggering behavior, not just parsing.

## Grammar and firing semantics, checked directly against real BuildKit source

`~/git/moby/vendor/github.com/moby/buildkit/frontend/dockerfile/
instructions/parse.go`'s own `parseOnBuild`, and `~/git/moby/daemon/
builder/dockerfile/dispatchers.go`'s own `initializeStage`/
`dispatchTriggeredOnBuild`:

* `ONBUILD <instruction>` stores `<instruction>`'s own **raw,
  unparsed** text (everything after the `ONBUILD ` prefix,
  case-insensitively stripped) — real BuildKit only inspects the
  wrapped instruction's own first word at declare time (to reject
  `ONBUILD`/`FROM`/`MAINTAINER` as triggers, see below); the rest is
  never parsed until the trigger actually fires, in a later, separate
  build. `oci-dockerfile`'s own `Instruction::Onbuild(String)` matches
  this exactly, and a new public `parse_onbuild_trigger` re-parses that
  raw text into a real `Instruction` at the point it's actually
  consumed.
* Rejected as a trigger, at declare time (real error wording matched
  verbatim): `ONBUILD ONBUILD ...` ("Chaining ONBUILD via `ONBUILD
  ONBUILD` isn't allowed"), `ONBUILD FROM ...`/`ONBUILD MAINTAINER ...`
  ("`FROM`/`MAINTAINER` isn't allowed as an ONBUILD trigger").
* **Firing**: the moment a *separate*, later build's own `FROM`
  resolves to an image carrying one or more stored triggers, they run
  — in the order they were declared — before any of that later
  build's own explicit instructions, and are then cleared from the
  resulting image's own config (never propagated past that one `FROM`
  unless the later build declares new `ONBUILD` triggers of its own).

## Implementation: no new plumbing needed beyond one prepend

`cmd_build`'s own per-stage loop already resolves `base_config` (either
from an external pull, `FROM scratch`, or an earlier in-memory stage)
before calling `build_stage`. This increment inserts one step right
there: `std::mem::take` the base's own `Config.OnBuild` list (firing
*and* clearing it in the same step — matching real Docker's own
"consumed exactly once" semantic exactly), re-parse each trigger via
`parse_onbuild_trigger`, and prepend the results to the stage's own
`instructions` list before it's built. Every existing mechanism
(`build_stage`'s own `needs_rootfs` detection, the local build cache,
layer commit) already handles an arbitrary instruction list correctly
with no further change — the prepended instructions are
indistinguishable from ones the child Dockerfile actually wrote once
they're in that list.

## A real bug caught by the existing test suite, not assumed away

Adding `ContainerConfig::on_build: Vec<String>` without
`deserialize_with = "null_as_default"` broke a real, existing fixture
test the moment the full workspace suite ran:
`parses_real_ubuntu_image_config_with_an_explicit_null_volumes_field`
failed with `"invalid type: null, expected a sequence"` — the *exact*
real `docker.io/library/ubuntu:24.04` config fixture already checked
into this repo has a literal `"OnBuild": null` (a nil Go slice, the
same real bug class `volumes`/`exposed_ports`/`labels` already needed
this exact fix for, `0` sessions ago). Fixed the same way; a proactive
identical fix was also applied to `HealthcheckConfig::test` (same
field shape/origin, same proven-real risk, no known fixture hitting it
yet but no reason to wait for one) — a new synthetic unit test exercises
it directly since no real image happened to combine a set `Healthcheck`
with an explicit `null` `Test`.

## Real, automated tests

7 new `oci-dockerfile` unit tests: raw-text storage verified verbatim
for both `RUN`/`COPY` triggers; missing-argument, chaining, and
`FROM`/`MAINTAINER`-as-trigger errors (case-insensitive keyword check
included); `parse_onbuild_trigger` itself re-parsing stored text into
real instructions, and surfacing a real parse error for garbage input.

2 new `ociman_build` integration tests, both real, end-to-end,
cross-build: `onbuild_trigger_fires_in_a_later_build_using_this_image_
as_its_base` — a first build declares `ONBUILD RUN echo hi >
/onbuild-marker.txt` (verified to *not* run in that build itself, and
to store exactly that one trigger string), a second, separate build
`FROM`s the first build's own result with no instructions of its own
at all, and the trigger actually fires: one real new layer, a real
file `ociman run --rm ... cat /onbuild-marker.txt` actually reads back
as `"hi\n"`, and the child's own config carries no `ONBUILD` of its own
(consumed, not propagated further). `onbuild_trigger_with_an_
unparseable_body_is_a_clear_error_when_it_fires` — a garbage trigger
(`ONBUILD FROBNICATE something`) is accepted at declare time (never
validated beyond its own keyword) but produces a real, clear build
error the moment a later build actually tries to fire it.

Plus 2 new `oci-spec-types` tests for the null-handling fix. All other
50 pre-existing `ociman build` tests and 46 pre-existing `oci-spec-
types` tests still pass unmodified. Full workspace `cargo build
--workspace --locked`/`cargo test --workspace --locked` (2 clean runs)/
`cargo fmt --all --check`/`cargo clippy --workspace --all-targets
--locked -- -D warnings` all clean.

## What this doesn't do yet

* `COPY --from=<stage>` reading from an *earlier stage* whose own base
  had `ONBUILD` triggers: those already fire correctly (the trigger-
  firing step runs for every stage, regardless of whether anything
  later reads from it via `COPY --from=`), so no gap here — noted only
  because it's worth being explicit that this wasn't missed.
* Heredocs, BuildKit-only flags, and `ARG`/`ENV` interpolation within
  other instructions' own arguments remain the last real gaps in
  `oci-dockerfile`'s own "deliberately not handled yet" list.
