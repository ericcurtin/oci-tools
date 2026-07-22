# Design note 0172: `ociman healthcheck run`

Status: implemented (one real, honestly-flagged gap: `Timeout` isn't
enforced yet — see "What this doesn't do yet")
Scope: `bin/ociman/src/main.rs` (`Command::Healthcheck`,
`HealthcheckCommand`, `cmd_healthcheck_run`, `healthcheck_exec_args`);
`tests/tests/ociman_healthcheck.rs`.

## A real, manual, one-shot invocation — not the scheduler

0116 already parses/stores a real `HEALTHCHECK` instruction's own
`Test`/`Interval`/`Timeout`/`StartPeriod`/`StartInterval`/`Retries`
fields byte-for-byte compatibly with real Docker's own wire format,
but explicitly left *running* one out of scope — this project's own
established "no periodic execution, no scheduler" boundary. Real
`podman healthcheck run CONTAINER` is a different, smaller thing than
that scheduler: a manual, single invocation of the already-stored
test, typically invoked externally (a systemd timer unit, in real
podman's own `generate systemd --new` output) rather than by podman
itself — this increment implements exactly that manual invocation,
staying inside the "no scheduler" boundary while still delivering
real value.

## First nested subcommand in this project

`ociman healthcheck run CONTAINER` is a real, two-level command
(matching real `podman healthcheck`'s own identical shape, which has
exactly one subcommand today too) — the first time this project's own
CLI needed one. A small `HealthcheckCommand` enum (`derive(Subcommand)`
on `Command::Healthcheck`'s own field) is all `clap`'s derive macro
needs for this; no new parsing infrastructure required.

## Resolving the container's own healthcheck: a frozen snapshot, not a
live re-read

The container's own already-recorded `ANNOTATION_IMAGE` (the exact
reference it was created from) is resolved the same way `cmd_diff`/
`cmd_commit` already do — a real, deliberate choice matching real
podman's own model: the container's own effective config is whatever
the image said *at creation time*, not a live re-read of a possibly-
since-changed or -removed image. If that image is no longer in local
storage at all, this is a real, clear error (`"container's own base
image is no longer in local storage"`), not a silent skip.

## `healthcheck_exec_args`: the one real new piece of translation logic

`HealthcheckConfig.test` is stored exactly as the real Docker wire
format has it (`["NONE"]`, `["CMD", ...]`, `["CMD-SHELL", "<command>"]`)
— 0116 already parses *into* that shape; nothing in this project could
translate it back *out* into real exec args until now.
`healthcheck_exec_args` does that: `CMD` is the remaining args
verbatim; `CMD-SHELL` wraps its one command string in `/bin/sh -c`
(checked directly against real moby's own `getShell`, `~/git/moby/
daemon/health.go`: real docker prefers a per-image `Config.Shell`
override first, which this project's own `ContainerConfig` has no
equivalent field for at all yet, then falls back to the identical
`/bin/sh -c` this project already uses); `NONE`, an empty `Test`, or
an unrecognized first element are all `None` — "no healthcheck to
run" — matching real moby's own `getProbe`'s identical permissive
fallback (an unrecognized kind logs a warning and is treated as no
healthcheck at all, never a hard parse error).

## Execution: `cmd_exec`'s own plumbing, unchanged

The test runs inside the container's own already-joined namespaces via
the exact same `oci_runtime_core::exec::ExecRequest`/`exec` `cmd_exec`
itself uses — same user/capabilities/`cwd`/env the container's own
init process has, no per-invocation overrides (a healthcheck test
always runs exactly the way the container's own main process does,
matching real docker/podman). `exit_code == 0` is healthy (nothing
printed, exit `0`); anything else prints `unhealthy` and exits `1`
(`0` with `--ignore-result`, matching real `podman healthcheck run
--ignore-result` exactly — the real status text is still printed
either way, only the exit code changes). A container that isn't
running at all prints `stopped` (matching real podman's own
`HealthCheckStopped` -> `"stopped"` string) instead of attempting to
exec anything.

## Verified against real `podman healthcheck run`

Real `podman build` (default OCI output format) refuses to store a
`HEALTHCHECK` at all ("`HEALTHCHECK is not supported for OCI image
format... Must use 'docker' format`" — a real, own limitation of real
podman's own OCI-manifest path, not something this project shares,
since this project's own store isn't manifest-format-constrained the
same way `HealthcheckConfig` is always stored regardless of what
format `ociman save` later exports to). Rebuilding the same test image
with `podman build --format docker` and running the identical
sequence against a real, live `podman healthcheck run` produced
exactly the same shape this implementation does: `unhealthy`
printed + exit `1` before the test file exists, nothing printed + exit
`0` once it does.

## Tests

Four new integration tests in `tests/tests/ociman_healthcheck.rs`: an
unknown container is a clear error; an already-stopped container
prints `stopped`; a container with no healthcheck defined is a clear
error; and the real, convincing end-to-end check — a genuinely running
container's real `HEALTHCHECK` actually exec'd twice (`unhealthy`
before a marker file exists, healthy/silent after), plus
`--ignore-result`'s own exit-code-only effect. Seven new unit tests
for `healthcheck_exec_args` covering all three real `Test` shapes plus
the `NONE`/empty/unrecognized-kind "nothing to run" cases. Full `cargo
build --workspace --locked`/`cargo test --workspace --locked` (2 clean
runs)/`cargo fmt --all --check`/`cargo clippy --workspace --all-
targets --locked -- -D warnings`/`python3 ci/guards.py`/`cargo deny
check`/`bash ci/native-ci.sh` all clean.

## What this doesn't do yet

* **`Timeout` is not enforced.** A genuinely hung healthcheck test
  currently blocks `ociman healthcheck run` itself rather than being
  killed and reported `unhealthy` after the configured timeout —
  `oci_runtime_core::exec::exec`'s own internal fork/wait shape (a
  double-fork when a PID namespace is joined, for the same reason
  `cmd_exec` itself already needs one) makes a precise, safe
  timeout-then-kill significantly more involved than this increment's
  own otherwise-narrow scope justified attempting in one sitting — a
  real, deliberately deferred gap, not a silently accepted one.
* No persisted health-check log/state, no startup-healthcheck
  distinction, no `--health-on-failure` actions — real podman's own
  separate, much larger subsystem (a real per-container log file,
  retry-streak tracking across repeated invocations, and automatic
  kill/restart/stop actions), entirely out of scope for a single
  manual invocation.
