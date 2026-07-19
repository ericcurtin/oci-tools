# Design note 0076: `ociman build --target` (milestone 4)

Status: implemented
Scope: `bin/ociman/src/main.rs` (new `--target` flag on `Command::
Build`), `bin/ociman/src/build.rs` (`cmd_build`'s own target
resolution), `tests/tests/ociman_build.rs`.

`--target` has been tracked as a "still open" item on `cmd_build`'s own
scope list since the very first multi-stage increment (0053) — every
increment since has explicitly called out that the target was always
the last stage in the file because the flag itself "doesn't exist
yet". This increment adds it.

## Almost entirely already built

`oci_dockerfile::stages_needed_for` (0043) has taken an explicit
`target: usize` stage index from the start — `cmd_build` was just
always passing `stages.len() - 1` for it. `oci_dockerfile::find_stage`
(also since 0043, case-insensitive name lookup) is exactly the
resolution a real `--target <name>` needs. The entire feature is one
`match` in `cmd_build` replacing that one hardcoded line, plus a CLI
flag threaded through — no new dependency-graph, pruning, or stage-
building logic needed at all.

## Checked directly against real BuildKit

`~/git/moby/vendor/github.com/moby/buildkit/frontend/dockerfile/
dockerfile2llb/convert.go`'s own `resolveTarget`:

```go
func (dctx *dispatchContext) resolveTarget() (*dispatchState, error) {
    if dctx.opt.Target == "" {
        return dctx.allDispatchStates.lastTarget(), nil
    }
    target, ok := dctx.allDispatchStates.findStateByName(dctx.opt.Target)
    if !ok {
        return nil, errors.Errorf("target stage %q could not be found", dctx.opt.Target)
    }
    return target, nil
}
```

Two things confirmed directly from this rather than assumed:

* An empty/absent `--target` resolves to the *last* stage — already
  exactly this project's own existing default.
* `findStateByName` (case-insensitively lowercased) is the *only*
  resolution path — there's no fallback to a numeric stage index the
  way `COPY --from=<N>` supports. An anonymous (unnamed) stage can
  never be targeted via `--target` in real BuildKit, matching this
  project's own `find_stage`, which already only ever matches a
  stage's own `Some(name)`.

The error message is copied verbatim (`target stage {name:?} could not
be found`, matching Go's own `%q` quoting via Rust's `{:?}` on a
`&str`) — a real, exact match rather than a paraphrase, confirmed by a
test asserting the literal string.

## What `--target` actually buys, beyond just "which config gets
tagged"

Because the resolved target index feeds straight into the existing
`stages_needed_for` pruning, **`--target` can build a Containerfile
that would otherwise fail outright** — any stage the target doesn't
transitively depend on (via `FROM`/`COPY --from=`) is never built at
all, even one with a broken/nonexistent base image. Verified both
ways: a real, manual end-to-end run building only an earlier `builder`
stage while a later `final` stage names a real image that doesn't
exist on the registry (a real `HTTP 401`/`UNAUTHORIZED` from the real
registry if it *were* built — confirmed by also running the exact same
Containerfile with no `--target` and watching it fail exactly that
way), and an automated test doing the same thing offline.

## Real, manual end-to-end verification before writing a single automated test

Built the release binary and ran three real scenarios against a real,
freshly-pulled `docker.io/library/busybox:latest`: `--target builder`
on a two-stage Containerfile whose second stage's `FROM` names a
genuinely nonexistent image — succeeded, and the built image's own
`/marker.txt` (written by the *targeted* stage's own `RUN`) read back
correctly; the same Containerfile with no `--target` at all — failed
with the real registry's own `401`, proving the second stage really
would have been attempted without pruning; `--target no-such-stage` —
failed with the exact `target stage "no-such-stage" could not be
found` message.

## Real, automated tests

`target_builds_only_the_named_stage_and_prunes_everything_after_it`
(mixed-case `--target BUILDER` against a stage declared `AS builder`,
confirming the case-insensitive match, with the same "later stage has
an unpullable base image" proof-of-pruning technique
`an_unreferenced_stage_is_pruned_and_never_built` already established)
and `target_naming_no_real_stage_is_a_clear_error` (asserting the
literal real-BuildKit-matching error message, and that a failed build
never leaves a partial image tagged, matching this test file's own
established convention for every other rejection test).

## Performance

Touches only `bin/ociman/src/build.rs`'s own target-stage resolution
and `main.rs`'s `Build` subcommand's own CLI parsing/dispatch — not
`cmd_run`/`synthesize_spec`/`resolve_seccomp`, `oci-runtime-core`, or
either cgroup driver (confirmed via `git diff --stat`), and none of
this is on the `ociman run`/`ocirun run` startup/destroy hot path this
project's own benchmarks measure. No benchmark re-verification needed,
consistent with every prior build-only increment.

## What's still not here

* The build cache — still nothing actually caches a previous build's
  own result yet, unchanged by this increment.
* `ONBUILD`/`HEALTHCHECK`, anonymous/untagged build mode — unchanged,
  tracked on `cmd_build`'s own module doc comment.
