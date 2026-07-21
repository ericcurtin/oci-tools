# Design note 0119: `ociman build`'s `RUN` steps see declared `ARG` values in their own shell environment

Status: implemented
Scope: `bin/ociman/src/build.rs` (`build_stage`'s own `current_args`
accumulator, `apply_instruction`'s `Instruction::Arg` arm,
`run_instruction`, new `build_arg_overlay`, `run_step_spec`);
`bin/ociman/src/build_cache.rs` (module doc comment fix — no code
change); `tests/tests/ociman_build.rs` (3 new tests).

## A real, previously-unnoticed correctness gap, found by checking documentation against actual behavior

While investigating the next candidate increment, `oci-dockerfile/src/
expand_stage.rs`'s own doc comment ("the shell running inside the
container does its own `$VAR` expansion at container-build time, using
the `RUN` step's own environment") was checked against what `build.rs`
actually did — and it didn't: `run_step_spec` only ever set a `RUN`
step's own process environment from the image's own already-*persisted*
`ENV` list, never from any currently-declared `ARG`. Confirmed directly,
not just by reading code: a real `ARG VERSION=1.0` + `RUN echo
"VERSION is [$VERSION]" > /result.txt` Containerfile produced `VERSION
is []` with `ociman build` — but `VERSION is [1.0]` with a real,
installed `podman build` against the *exact same* Containerfile. A
real, current parity gap, not a hypothetical one — `ARG`s referenced
directly inside a `RUN` command (a common real-world pattern, e.g.
`ARG VERSION\nRUN wget .../v$VERSION/...`) silently produced wrong
output.

## Fix, checked directly against real BuildKit source

`~/git/moby/daemon/builder/dockerfile/dispatchers.go`'s own
`dispatchRun`: `buildArgs := d.state.buildArgs.FilterAllowed(
stateRunConfig.Env)` computes every currently-declared `ARG` (already
resolved — override, inline default, or inherited meta-arg) whose name
isn't *already* a real `ENV` key, then `withEnv(append(stateRunConfig
.Env, buildArgs...))` injects exactly that set into the `RUN` step's
own temporary process environment — never persisted back into the
image's own final `ENV` (an `ARG`'s value only survives past the build
if a later `ENV` instruction explicitly re-declares it).

Ported directly: `build_stage`'s own per-stage instruction loop now
threads a `current_args: Vec<(String, String)>` accumulator (stage-
local, reset per stage, matching real Docker's own per-stage `ARG`
scoping already established by `expand_stage`) through every
`apply_instruction` call. `Instruction::Arg`'s own handling — previously
a pure no-op, since `expand_stage` had already fully resolved every
`ARG`'s own value before `build.rs` ever sees it — now records each
resolved `(name, value)` pair into that accumulator (a bare `ARG NAME`
with no default and no matching meta-arg resolves to `None` and is
correctly never added — nothing to inject). `run_instruction` computes
the real overlay (`build_arg_overlay`: every `current_args` entry not
shadowed by a real `config.config.env` key, matching `FilterAllowed`
exactly) and `run_step_spec` appends it to `process.env` — real,
observable `$VAR` expansion inside the `RUN` step's own shell, with
zero effect on the image's own persisted `ENV`.

## A real build-cache correctness bug this same change would otherwise have introduced — caught before it shipped, not after

`RUN`'s own build-cache key (`bin/ociman/src/build_cache.rs`) is just
its `created_by` string — the literal, *unexpanded* command text (`RUN
echo $VERSION`, verbatim, since `expand_stage` never touches it).
Without a further change, two builds of the same Containerfile with
*different* `--build-arg VERSION=` values would produce the exact same
`created_by` string despite the `RUN` step now genuinely seeing (and
producing real output based on) different environment values — a
stale cache hit reusing the wrong build's own layer. Fixed the same way
real Docker's own classic builder does (`prependEnvOnCmd`, visible in a
real `docker history` as `RUN |1 VERSION=1.0 /bin/sh -c ...`):
`run_instruction` folds the same `arg_overlay` into `created_by` as a
real prefix (`RUN |1 VERSION=1.0 echo ...`) *before* computing the
cache key, so a different `--build-arg` value produces a genuinely
different key, correctly busting the cache. Verified directly, not
just reasoned about: a dedicated test builds the same Containerfile
twice with different `--build-arg` values and asserts the two
resulting images' own last layer digests differ (would fail against
the pre-fix, unprefixed cache key).

`build_cache.rs`'s own module doc comment previously claimed `RUN`'s
own `created_by` "already reflects any `--build-arg`/`ARG`/`ENV`
substitution actually used inside the command line itself... `expand_
stage`'s own `$VAR` substitution runs before `RUN`'s own text is ever
seen here" — this was simply wrong (the exact inverse of `expand_
stage`'s own explicit, correct doc comment on the same point), likely
written before this gap was ever noticed. Corrected to describe what
this increment actually implements.

## Real, automated tests

Three new `ociman_build` integration tests: `run_step_sees_a_declared_
args_own_value_in_its_own_shell_environment` (the exact real bug above,
now fixed, plus confirming the value never leaks into the built
image's own persisted `ENV`); `build_arg_override_changes_what_run_
sees_and_busts_the_cache` (two builds, different `--build-arg` values,
real different output *and* real different layer digests — not a
stale cache hit); `arg_never_overrides_an_explicit_env_with_the_same_
name_in_a_run_step` (real `FilterAllowed` precedence rule). All 50
pre-existing `ociman build` tests still pass unmodified. Full `cargo
build --workspace --locked`/`cargo test --workspace --locked` (2 clean
runs)/`cargo fmt --all --check`/`cargo clippy --workspace --all-targets
--locked -- -D warnings` all clean.

## What this doesn't do yet

* `CMD`/`ENTRYPOINT`/`HEALTHCHECK CMD` don't run at build time at all
  (they only ever set config defaults for the eventual container), so
  they have no analogous "sees `ARG` values" concern the way `RUN`
  does — nothing to do there.
* `COPY --chown=$SOME_ARG` and friends already worked before this
  increment (`expand_stage`'s own build-time `$VAR` string substitution
  covers `COPY`/`ADD`'s own flags/sources/dest directly) — this
  increment is specifically, only, about `RUN`'s own runtime shell
  environment, the one place build-time text substitution was
  deliberately never going to be the right mechanism.
