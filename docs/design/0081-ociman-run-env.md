# Design note 0081: `ociman run -e/--env` (milestone 3)

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Run`'s new `env` flag,
`cmd_run`/`synthesize_spec`'s new parameter, `cmd_exec`'s own existing
`--env` handling switched to the same helper), `bin/ociman/src/
build.rs` (`apply_env_overrides`, `set_env_var` promoted to
`pub(crate)`), `tests/tests/ociman_run.rs`, `tests/tests/
ociman_exec.rs`.

`ociman run` had no way to set an environment variable at all —
`ociman exec --env` already did (matching real `podman exec -e`), a
direct, already-shipped precedent for the same real gap in `run`.
Matches real `docker run -e`/`podman run -e` exactly, including the
bare-name pass-through-from-the-calling-process convention this
project's own `--build-arg` already established (`parse_build_args`).

## A real, pre-existing correctness bug found while reusing the precedent, not introduced by this increment

`ociman exec --env`'s own existing implementation just appended the
override onto the container's own process env list
(`effective_env.extend(extra_env.iter().cloned())`) rather than
replacing an already-present entry for the same name. That's a real
bug, not a style choice: a real container process's own `getenv(3)`
scans `environ` from the start and returns the *first* match
(`man 3 getenv`'s own documented linear-scan behavior) — so overriding
`PATH` this way would silently have **no effect** on anything inside
the container that actually calls `getenv`, even though the override
value genuinely made it into the array. Confirmed directly (not just
reasoned about) with a new test asserting the exact stdout of a real
`echo $PATH` inside a real running container.

`set_env_var` (already used by `ociman build`'s own Dockerfile `ENV`
instruction handling) already does this correctly — replace an
already-present `KEY=` entry in place, append only when the name is
genuinely new. Promoted to `pub(crate)` and built on with a new
`apply_env_overrides` (parses `KEY=value` vs. bare `KEY`, same
bare-name-pulls-from-the-process-environment convention
`parse_build_args` already established for `--build-arg`), used by
**both** `ociman run`'s new `-e`/`--env` and a fix to `ociman exec
--env`'s own pre-existing append-only bug — one shared, correctly-
checked primitive instead of two independently-maintained (and, until
now, differently-buggy) copies of the same idea.

## Real, manual end-to-end verification before writing a single automated test

Built the release binary and ran `ociman run --rm -e PATH=/custom/bin
-e GREETING=hello docker.io/library/busybox:latest -- /bin/sh -c
'echo $PATH $GREETING'` against a real, freshly-pulled `busybox` (whose
image config already sets its own `PATH`) — printed `/custom/bin
hello`, confirming both the override-in-place (not a duplicate,
shadowed entry) and the new-variable-append cases work correctly
together in one real invocation.

## Real, automated tests

`run_env_flag_overrides_an_existing_variable_and_adds_a_new_one` (a
real running container, checked via its own real stdout, the same
"actually printing `$PATH` from inside the container" proof the manual
verification used, not just inspecting the written spec) and a new
`exec_env_flag_overrides_an_existing_variable_in_place` alongside the
already-existing (and still passing, unmodified in its own
expectations) `exec_cwd_and_env_flags_override_the_defaults` — the
existing test only ever exercised a genuinely new variable name, so it
never actually covered the override-collision case this increment
fixes; the new test does, directly. `apply_env_overrides`'s own new
unit tests cover all four cases: override-in-place, append-new,
bare-name-from-environment, and bare-name-unset-is-dropped.

## Performance — hot-path change, A/B re-verified

Touches `main.rs`'s own `synthesize_spec` directly, so a `git stash`/
`git stash pop` A/B `hyperfine` comparison was run (same methodology
as 0080): noise-dominated as expected (`before` measured 1.06×
"faster" than `after`, well within one stddev — every comparison at
this scale in this project's own history flips which binary "wins").
No plausible regression mechanism: `apply_env_overrides` is a no-op
linear scan over a typically tiny (single-digit) environment list,
and does nothing at all when no `-e` flag is given.

## What's still not here

* `-e`/`--env` for `ociman build`'s own `RUN` steps — a real,
  separate, smaller future increment (Dockerfile `ENV` already covers
  the common "set an env var for the rest of the build" case;
  `--build-arg`-style per-invocation overrides for a build's own `RUN`
  environment are a different, narrower feature).
* `--hostname`, `--workdir` (CLI overrides), `-v`/`--volume`,
  `--entrypoint`, `ocirun update`/`--pid-file` — other, still-open
  small CLI gaps from the same survey that led to this increment and
  0080.
* The build cache, `ONBUILD`/`HEALTHCHECK`, anonymous/untagged build
  mode, `createContainer`/`startContainer` hooks, automated
  failed-systemd-scope cleanup — unchanged, unrelated leftovers from
  earlier milestones.
