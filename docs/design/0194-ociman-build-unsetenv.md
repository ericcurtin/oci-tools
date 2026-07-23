# Design note 0194: `ociman build --unsetenv`, and a real stray-`TERM`
fallback bug found while verifying it

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Build`'s new `--unsetenv`
flag, `DEFAULT_ENV_WHEN_IMAGE_DECLARES_NONE`, `synthesize_spec`'s env
fallback fix); `bin/ociman/src/build.rs` (the `--unsetenv` application,
`run_step_spec`'s identical env fallback fix); `tests/tests/
ociman_build.rs`.

## Continuing milestone 4

Checked real `docker build --help`/`podman build --help` directly:
both support `--unsetenv` ("unset environment variable from final
image"); podman additionally has `--unsetlabel` (not implemented
here — a natural, small future companion). Checked the exact real
semantics directly before implementing anything: `--unsetenv FOO`
removes `FOO` from the *final* image's environment regardless of
whether it came from the base image's own inherited config or any
`ENV` instruction in the Containerfile, applied once at the very end
(so a variable re-declared by a *later* `ENV` is still removed), and
— unlike `--label`, which adds its own extra `LABEL` history entry —
produces no history entry of its own at all.

## The fix

`Command::Build` gains `--unsetenv <NAME>` (repeatable, bare names
only). Applied right after the existing `--label` step (same "after
every real instruction" timing, same code region), filtering
`ContainerConfig::env` by key, with no `record_empty_history` call —
matching the checked-directly no-history-entry behavior.

## A second, real, previously-unnoticed bug found while verifying this
end to end

Manually testing `--unsetenv` on the *only* variable a base image
declares (emptying `Config.Env` down to zero entries — the first
thing that actually makes this reachable in practice; almost every
real base image declares at least a `PATH`) revealed a real
discrepancy: the resulting container showed `PATH=...` *and* a stray
`TERM=xterm`, while a real `podman run` against the exact same
scenario shows only `PATH=...` (confirmed directly, side by side: the
`Config.Env` inspected on both is genuinely empty in both cases, so
this fallback is happening in the *container-engine* layer — real
podman's own `libpod`/`specgen`, not `crun`/`runc` themselves — which
this project's own `ocirun` intentionally has no equivalent of at all,
matching real `crun run`/`runc run` exactly).

Root-caused directly: both `synthesize_spec` (`bin/ociman/src/
main.rs`, used by `ociman run`/`create`) and `run_step_spec`
(`bin/ociman/src/build.rs`, used for `RUN` step execution during a
build) had `if !container_config.env.is_empty() { process.env =
container_config.env; }` — silently leaving `process.env` at
whatever `Spec::example()`'s own placeholder default already was
(`PATH=...` *and* `TERM=xterm`, the real upstream OCI runtime-spec's
own illustrative example, correct for `ocirun spec`'s own real-runc-
compatible template) whenever the image declared *no* env at all,
rather than replacing it outright. Before `--unsetenv` existed, this
was reachable only for an image whose base already declared zero env
vars — rare in practice — so it went unnoticed.

Fixed by defining an explicit `DEFAULT_ENV_WHEN_IMAGE_DECLARES_NONE`
(just the real `PATH` string, matching real podman's own directly-
confirmed fallback), used unconditionally in both call sites instead
of falling through to `Spec::example()`'s own full placeholder list —
the container's own `process.env` is now always either the image's
real, declared env, or (only when that's genuinely empty) just this
one real, podman-matching `PATH` fallback, never both a real value and
a leftover template artifact at once.

## Tests

Four new integration tests in `tests/tests/ociman_build.rs`: removing
a declared var while leaving another untouched; removal winning over
a later re-declaring `ENV`; no history entry added; and the
stray-`TERM` fallback fix itself (checked via a real `ociman run ...
env`, not just the persisted config, since the bug was specifically
about the *runtime* fallback, not the stored manifest). Two of these
four initially had real test bugs of their own — an overly strict
exact-stdout-equality assertion that didn't account for the shell's
own legitimate `SHLVL`/`PWD` additions, and an `Option::unwrap()` on
a `Env` field that's legitimately *absent* (not an empty array) once
`--unsetenv` removes an image's only declared variable — both found
and fixed by actually running the tests rather than assuming they'd
pass. All 95 pre-existing `ociman build` tests, all `ociman run`
tests, and all `ocirun run` tests (confirming `ocirun`'s own separate,
unaffected code path) continue to pass unchanged. Full `cargo build
--workspace --locked`/`cargo test --workspace --locked` (2 clean runs,
83/83 result blocks)/`cargo fmt --all --check`/`cargo clippy
--workspace --all-targets --locked -- -D warnings`/`python3
ci/guards.py`/`cargo deny check`/`bash ci/native-ci.sh` all clean. No
performance regression (`ociman run --rm` ~64ms with adequate warmup,
consistent with prior baselines — an early single, noisy 80ms reading
was confirmed to be system noise, not a real regression, by re-running
with more warmup iterations).

## What this doesn't do yet

`--unsetlabel` (real podman's own separate flag for unsetting an
inherited `LABEL`, distinct from `--unsetenv`) remains unimplemented —
a natural, small, well-scoped future companion following the exact
same pattern established here.
