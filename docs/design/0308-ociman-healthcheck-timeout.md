# Design note 0308: `ociman healthcheck run` enforces `HealthcheckConfig.timeout`

Status: implemented
Scope: `crates/oci-runtime-core/src/process.rs`,
`crates/oci-runtime-core/src/exec.rs`, `bin/ociman/src/main.rs`,
`bin/ocirun/src/main.rs`, `bin/ocicri/src/launcher.rs`,
`tests/tests/ociman_healthcheck.rs`.

## The gap

`docs/design/0172` shipped `ociman healthcheck run`'s core effect but
honestly flagged one real, deliberately-not-silently-dropped gap: the
image-declared `HEALTHCHECK`'s own `Timeout` was never enforced — a
genuinely hung test blocked the command itself indefinitely instead of
being killed and reported `unhealthy`, matching real `docker`/`podman
healthcheck run`'s own timeout-means-unhealthy semantics. Reconfirmed
live in `--help`/doc-comment text as of `0307`, seven increments later
— never picked up until now.

## Why this was deferred: the PID-namespace relay

`oci_runtime_core::exec::exec` almost always forks *twice* for a real
container (this project's own `synthesize_spec` always includes a PID
namespace): an outer child joins every other namespace via `setns(2)`,
then forks an *inner* child specifically so it's born inside the
joined PID namespace (`setns(CLONE_NEWPID)` never moves the calling
process itself, only its own subsequently-forked children — the same
reason `crate::launch`'s own container-creation path needs the
identical two-fork shape). The outer child blocks on a plain
`process::wait` for the inner one, then relays its exit status back up
to `exec`'s own top-level caller.

A timeout mechanism that only kills the *outer* relay would not
actually stop the real, hung test process running inside the joined
namespace — it would become an orphan and keep running regardless.
Correctly enforcing a deadline means killing the *inner* process,
which requires the kill logic to live inside the same forked process
that already knows its pid, not bolted on from the top-level caller.

## Implementation

`ExecRequest` gained a `timeout: Option<std::time::Duration>` field.
`exec()`'s own decision to fork the inner relay child now also
triggers whenever a timeout is given, not just when a PID namespace
needs joining — so the same relay branch (which already has the inner
child's real pid in scope) is where the deadline gets enforced,
regardless of which namespaces the target container actually has.

A new `process::try_wait` (`WNOHANG`-based, non-blocking `waitpid`)
gives `exec.rs`'s new `wait_with_deadline` the same "poll, kill + reap
on deadline" shape `hooks::wait_with_timeout` already established for
a hook's own `Timeout` (there, polling `std::process::Child::
try_wait`; here, a bare `fork`ed pid with no such handle at all).
`None` waits forever, identical to `exec`'s own pre-existing behavior
— every caller except `ociman healthcheck run` passes `None`
(`ocirun exec`/`ociman exec`, matching real `crun exec`/`runc exec`/
`podman exec`'s own identical lack of a `--timeout` flag, checked
directly; `ocicri`'s own `ExecSync` helper already enforces its own
`ExecSyncRequest.timeout` at the process-group level via a separate,
pre-existing mechanism, so a second, inner deadline there would be
redundant).

On timeout, the killed process reports the same `128 + SIGKILL` code
`process::exit_code_from_wait_status` already produces for any other
signal-killed process — `cmd_healthcheck_run`'s own existing "nonzero
means unhealthy" check needed zero code changes to correctly treat
this as `unhealthy`, not a crash.

`HealthcheckConfig.timeout` is `0` when the Dockerfile's own
`HEALTHCHECK` never declared one (matching real Docker's own wire
format, which never bakes in a default at build time). Real podman's
own `DefaultHealthCheckTimeout` (`~/git/podman/libpod/define/
healthchecks.go`: `"30s"`) is normally baked into a *container's* own
persisted config at `create` time (`specgen`) — this project has no
equivalent persisted-resolved-healthcheck-config step yet, so the
identical real default is applied directly in `cmd_healthcheck_run`
instead, the one place that actually needs it.

## Verified

Manual, end-to-end (real seeded busybox image via `ociman build`):
`HEALTHCHECK --timeout=2s CMD sh -c "sleep 30; exit 0"` — `ociman
healthcheck run` printed `unhealthy` and returned in ~2.0s, not the
full 30s the test itself sleeps for. A healthy, fast `HEALTHCHECK CMD
true` (no `--timeout` declared at all) still completes in
milliseconds, confirming the new 30s default never adds artificial
delay to the common, already-fast case.

Integration (`tests/tests/ociman_healthcheck.rs`, 1 new test):
`healthcheck_run_kills_a_hung_test_once_its_own_timeout_elapses` — a
real container whose `HEALTHCHECK` sleeps 30 seconds but declares a 1
second `--timeout` is confirmed `unhealthy`, with the whole command
completing well under 15 seconds (a generous bound next to the
declared 1s deadline, accounting for real scheduling/test-host
variance, still far short of the full 30s the test itself would
otherwise take).

Unit tests: `process::try_wait` (2 new: still-running reports `None`,
then reports the real exit status once the child actually exits) plus
`oci-runtime-core`'s own existing 200 unit tests, unmodified.

Regression: all 5 `ociman_healthcheck.rs` tests pass (4 pre-existing +
1 new); all 202 `oci-runtime-core` unit tests pass (200 pre-existing +
2 new); full `cargo test --workspace --locked` (112 test result
blocks, 0 failures).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).

Performance: `exec()`'s own hot path (the common, no-timeout case
every existing caller — `ocirun exec`, `ociman exec`, `ocicri`'s own
`ExecSync`) is completely unaffected: `wait_with_deadline` with `None`
is a direct pass-through to the exact same `process::wait` call
`exec()` already made before this change, with no new syscalls, no
new fork (the PID-relay decision is unchanged unless a timeout is
actually given), and no polling loop entered at all. Only `ociman
healthcheck run` (never part of any benchmarked hot path in
`docs/benchmarks.md`) pays the new poll-with-deadline cost, and only
when it actually needs to.

## Still ahead

No further `ociman healthcheck run` gap is known against real `podman
healthcheck run`, beyond the already-documented, deliberately larger
features (`0172`'s own persisted health-check log/retry-streak
tracking, `--health-on-failure` actions, and a separate startup-
healthcheck variant this project's own `HealthcheckConfig` has no
field for at all).
