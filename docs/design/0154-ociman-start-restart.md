# Design note 0154: `ociman start`/`ociman restart`

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Start`/`Command::Restart`
CLI variants and dispatch; new `cmd_start`/`cmd_restart`; new shared
`launch_detached_and_confirm`, factored out of `cmd_run`'s own detach
branch; new shared `stop_container`, factored out of `cmd_stop`; new
`wait_for_keeper_to_finalize`, closing a real race — see below);
`tests/tests/ociman_start.rs` (6 new integration tests).

## What these do

`ociman start <id>`: re-run an already-`Stopped` container again,
reusing its own already-on-disk `config.json`/`rootfs/` exactly as
`run` originally left them — no re-extraction, no re-resolving the
original image reference, no re-writing `/etc/hosts` (0147) or the
base `diff` snapshot (0149): everything about the container's own
bundle is already real, valid, and completely unchanged since it was
first created. Always detached, matching real `podman start`'s own
real, checked-directly default (`~/git/podman/cmd/podman/containers/
start.go`: only `-a`/`--attach`, not given by default, streams the
container's own output live and blocks). A clear, real error for
anything other than a `Stopped` container, matching real `podman
start`'s own identical refusal to start an already-`Running` one
(`~/git/podman/libpod/container_internal.go`'s own `prepareToStart`:
`ErrCtrStateRunning`).

`ociman restart <id>`: stop the container first (same signal/timeout
escalation as `ociman stop`, real `SIGTERM`, matching real podman's
own default) if it's currently running, then start it again — matching
real `docker restart`/`podman restart` exactly (checked directly: stop
only if actually running, start regardless of the resulting state). A
no-op-then-start for an already-stopped container (nothing to stop
first). Prints the container id exactly once, at the very end, not
once for the stop half and again for the start half (same reasoning
`remove_container`'s own doc comment already established for
`cmd_rm`/`cmd_rmi --force`).

## Refactoring: two new shared helpers, no behavior change on their own

`launch_detached_and_confirm` was extracted verbatim from `cmd_run`'s
own `-d` branch (fork a keeper process, detach it from the controlling
terminal, run the container to completion via `run_and_finalize` in
that keeper, then block the caller until it reports a real pid or a
clear reason it never did) — both `cmd_run -d` and `cmd_start` need
the exact same "launch in the background, confirm it actually started,
print the id back" sequence. `cmd_run`'s own detach branch now just
calls it. Confirmed via the existing `ociman_run`/`ociman_logs`/
`ociman_stats`/`ociman_pause`/`ociman_top` test suites (all still pass
unmodified) that this refactor changed nothing observable.

`stop_container` was extracted from `cmd_stop`'s own body (same kill/
resend/escalate-to-`KILL` logic, same early-return points), with its
`println!(id)` calls removed so `cmd_restart` can call it without
double-printing; `cmd_stop` is now a thin wrapper:
`stop_container(id, time_secs, signal)?; println!("{id}"); Ok(())`.
Confirmed via the existing `ociman_stop`/`ociman_ps` test suites (still
pass unmodified) that this refactor alone changed nothing observable.

## A real, reproducible race found and fixed before committing to this design

A throwaway integration test (`ociman run` a container whose command
appends one line to a marker file then exits immediately, poll until
`Stopped`, `ociman start` it, poll until `Stopped` again, `ociman
restart` it, poll until `Stopped` a third time, then assert the marker
file has exactly three lines) failed **the majority of repeated runs**
(well over half) before the fix below: the marker file only ever showed
two lines after `restart`, even though `restart` itself reported
success and the polled status sequence after issuing it went straight
to `"stopped"` on the very first check.

Root cause, found via targeted tracing (temporary `eprintln!`/file-
based debug logging, all removed before this commit — a detached
container's own keeper process has its stdio redirected to `/dev/null`,
so ordinary `eprintln!` in code that runs inside it is silently
swallowed; a separate temp-file-based trace was needed to see it):
`PersistedState::effective_status()` (`crates/oci-runtime-core/src/
state.rs`) reports `Status::Stopped` whenever the container's own
recorded pid is no longer alive, **regardless of the raw, on-disk
`status` field** — a correct and useful heuristic for `ps`/`inspect`
display (a container's process can die on its own between polls, well
before anything gets around to writing the terminal state), but it
does **not** mean the container's own detached keeper process (the one
blocked in `run_and_finalize`, which forked it, still waiting on it and
about to run `reset_failed_systemd_scope` plus write the final
`Status::Stopped` record) has actually finished its own trailing
bookkeeping yet.

`stop_container`'s very first branch — `if state.effective_status() ==
Status::Stopped { return Ok(()); }`, meant purely as a fast, idempotent
no-op for an already-fully-at-rest container — could therefore return
successfully while the *previous* launch's own keeper was still
in-flight. `cmd_restart` then immediately called `cmd_start`, which
overwrote the persisted state with a fresh `Status::Creating` and
launched a **new** keeper — and moments later, the **old** keeper,
finally getting scheduled, ran its own already-queued "write
`Status::Stopped`" step and silently clobbered the new keeper's fresh
`Creating`/`Running` state with a stale one belonging to the dead pid
from the *previous* run. The new keeper's own real container process
kept running and eventually exited/wrote its own real state
correctly, but not before `wait_for_detached_container_to_start`'s
poll loop (waiting only for "not `Creating`", a condition the *stale*
clobbering write also satisfied) had already returned control to
`cmd_start`/`cmd_restart`, which had already returned to the caller —
so any observer polling status immediately after `restart` returns
could see `"stopped"` well before the actual third run had even
started, let alone finished writing its own marker-file line.

Real crun/podman's own containers don't have exactly this shape (a
`conmon`-style supervisor rather than an in-process forked keeper), so
there was no existing upstream implementation to check this exact race
against — it's a genuine consequence of this project's own detached-
keeper design, found and fixed through direct, repeated reproduction
rather than assumed.

Fix: a new `wait_for_keeper_to_finalize(containers, id)` helper, bounded
to 2 seconds (the keeper's own remaining work once its child has
already exited is normally near-instant; this must never hang forever
if something upstream left a stale `Running`/`Creating` record behind
with no keeper left to ever finalize it), polling the container's own
*raw* persisted status until it's no longer `Running`/`Creating`
(a no-op if it's already genuinely `Stopped`). Called at **every** point
that was previously an unconditional "the container's process is no
longer alive, so it's done" return: `stop_container`'s own initial
`effective_status() == Stopped` fast path, its two later "process died
during/after the graceful window" returns, and its final "escalated to
`KILL`" fallthrough — as well as directly in `cmd_start` itself (a
plain `ociman start` on a container whose pid just died naturally,
independent of `stop`/`restart` entirely, has the exact same race:
proceeding straight to a fresh `Creating` write without waiting first
would let that same container's own previous keeper's delayed terminal
write clobber it).

## Real, automated tests

Six new integration tests in `tests/tests/ociman_start.rs`:
`start` re-runs an already-stopped container (marker file goes from one
line to two); `restart` re-runs an already-stopped container a third
time (the exact scenario the race above was found in — reproduced
reliably before the fix, passing consistently across 20+ repeated runs
after it); `restart` on a genuinely still-running long-lived container
(stops the old process, confirms a new, different pid afterward, still
`running`); `start` on an already-running container is a clear error;
`start`/`restart` of an unknown container id are clear errors.

Full `cargo build --workspace --locked`/`cargo test --workspace
--locked` (2 clean full-workspace runs, plus 20+ repeated standalone
runs of the new `ociman_start` tests specifically to confirm the race
fix's stability)/`cargo fmt --all --check`/`cargo clippy --workspace
--all-targets --locked -- -D warnings`/`python3 ci/guards.py`/`cargo
deny check`/`bash ci/native-ci.sh` all clean.

## What this doesn't do yet

* **`-a`/`--attach`/`-i`/`--interactive`** for `start`/`restart`
  (streaming the container's own output live and waiting for it,
  rather than always detaching) — a real gap, deferred to a future
  increment; real podman's own default (this increment's own only
  supported mode) is detached either way, so this is a narrowing, not
  a behavior mismatch for the common case.
