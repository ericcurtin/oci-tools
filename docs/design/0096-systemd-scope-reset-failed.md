# Design note 0096: automated failed-systemd-scope cleanup (milestone 3)

Status: implemented
Scope: `crates/oci-runtime-core/src/systemd_cgroup.rs` (new
`reset_failed_unit`, `reset_failed_unit_dbus_roundtrip`,
`RESET_FAILED_TIMEOUT`), `bin/ociman/src/main.rs` (`cmd_run`/
`cmd_stop`/`cmd_rm`'s new cleanup calls, `reset_failed_systemd_scope`
helper).

The last "known, not-yet-handled edge case" `systemd_cgroup.rs`'s own
module doc has carried forward, completely untouched, since 0033
(repeated as a "known gap" bullet in 11+ design notes since): a
transient container scope that ends up in systemd's own `failed`
substate — rather than being auto-removed the way a normally-exited
scope already is — needs an explicit `ResetFailedUnit` D-Bus call to
actually get garbage-collected, and nothing in this project ever made
that call. Re-assessed via a fresh survey as genuinely smaller than
its own "carried forward 11 times" reputation suggested, and it was:
the fix itself is one new, small, self-contained function plus three
one-line call sites.

## Verified against real crun source first, not assumed

Read `~/git/crun/src/libcrun/cgroup-systemd.c` directly:
`reset_failed_unit(bus, unit)` is a single, synchronous
`ResetFailedUnit` D-Bus method call (no job to wait for, unlike
`StartTransientUnit`) — real crun calls it in two places: on a
`StartTransientUnit` failure (to clear a stale failed unit blocking a
retry), and **unconditionally at scope-teardown time**, with its
return value completely discarded either way (`reset_failed_unit(bus,
scope);`, no error check at the call site at all).

## The D-Bus call's own real behavior, confirmed directly, not assumed

Two real, distinct outcomes checked directly via `busctl --user call
... ResetFailedUnit s <name>` before writing any code:
* A unit name that was **never loaded at all** (fully garbage-collected,
  or never created) — a real, returned D-Bus error ("Unit ... not
  loaded").
* A unit that's loaded (even a scope that just exited normally,
  moments earlier, not actually in `failed` state) — a real **success**,
  a harmless no-op. `ResetFailedUnit` is not "fail if the unit wasn't
  actually failed" — it's "clear whatever failed-state bookkeeping
  exists for this unit, if any," which succeeds regardless.

This means, in practice, most real calls from this project's own three
new call sites return `Ok(())` (a scope still technically "loaded" in
some transitional state right after its own process exits) rather than
hitting the "not loaded" error branch at all — both are handled, and
neither is treated as a real failure.

## `reset_failed_unit`: same defensive shape as `create_scope`, deliberately simpler

A dedicated background thread + bounded `recv_timeout` (same pattern
`create_scope` already established, for the same reason: never let a
D-Bus interaction hang the calling process indefinitely) — but with a
shorter timeout (`RESET_FAILED_TIMEOUT`, 5s vs. `create_scope`'s own
10s `JOB_WAIT_TIMEOUT`) and no `JobRemoved` signal subscription at all,
since there's no asynchronous job to wait for here. Fully infallible
from the caller's own point of view (`pub fn reset_failed_unit(unit:
&str)`, no `Result` at all): every outcome (success, "not loaded", a
genuine D-Bus error, or a timeout) is logged at `debug` and never
propagated — this exists purely to clean up a real, if rare, resource
leak, never to affect whatever the caller is otherwise doing.

## Three call sites, chosen by where a container's process is actually confirmed to have stopped

Real crun's own single "scope-teardown" call site doesn't map onto a
single point in this project's own architecture, which has three
separate places a container's process actually stops:

* `cmd_run`'s own foreground wait returning (the common, successful
  path) — unconditional, matching crun's own "at teardown, regardless"
  reasoning.
* `cmd_stop`, once the container is confirmed no longer alive (either
  the graceful-signal path or the `KILL` escalation) — the most likely
  *real* trigger for a scope ending up abnormally `failed` in
  practice, since `stop`'s own bounded-wait-then-poll loop is a
  wholly separate `ociman` invocation from whatever `run` was.
* `cmd_rm --force`'s own kill-then-poll path, for the same reason as
  `stop`.

Deliberately **not** added to `cmd_kill`: it sends exactly one signal
and returns immediately, without ever confirming the process actually
died — calling `reset_failed_unit` at that point would be premature
almost every time (systemd hasn't even had a chance to notice the
process is gone yet), unlike `stop`/`rm --force`, which both already
wait.

## Real, manual verification against a real, freshly-pulled busybox

Built the release binary and exercised all three real call sites: a
normal `run` to completion, `stop` on a real running container, and
`rm --force` on a real running container — all three completed
correctly and quickly (no added hang or noticeable latency spike in
any manual timing). Separately confirmed, via `busctl` directly, both
of the D-Bus call's own real outcomes described above.

## Real, automated tests — and an honest limit on what's actually testable

One new unit test in `systemd_cgroup.rs`: `reset_failed_unit` against
a unit name that was never created at all completes well within its
own timeout against a real, reachable `systemd --user` session — the
plumbing itself, proven real. Deliberately **not** attempted: an
automated test that reliably forces a scope into `failed` substate on
demand. This module's own doc comment already flags this as real,
engineering-hard flakiness, not a straightforward repro — confirmed
directly while writing this increment (a self-`SIGKILL`led scope, the
first real attempt, self-cleaned normally rather than ending up
`failed` at all). Inventing a flaky test to force this exact substate
would violate this project's own established preference against
flaky tests; the existing, already-passing `ociman_run.rs`/
`ociman_stop.rs`/`ociman_kill.rs`/`ociman_rename.rs` integration
suites already provide real regression coverage that the three new
call sites don't break the ordinary run/stop/rm flows at all.

## Performance — `ociman run` is an explicitly benchmarked command, A/B re-verified

`cmd_run`'s own new call happens synchronously, on its own foreground
exit path, before the process actually terminates — a real, unavoidable
D-Bus round trip cost (real crun pays the exact same cost at its own
teardown point; making this call truly fire-and-forget would mean the
background thread gets killed by `cmd_run`'s own `std::process::exit`
before the D-Bus interaction has a real chance to complete, defeating
the entire point of the cleanup). A `git stash`/`git stash pop` A/B
`hyperfine` comparison against `ociman run --rm` (a hookless, ordinary
container) showed the *direction* flip between two separate runs
(1.08× "before" faster, then 1.11× "after" faster on a larger sample)
— consistent with `ociman run`'s own already-documented 33-80ms wide
contention-noise band (this project's own session notes: "never a
real regression found across ~20 A/B re-verifications"), not a real,
reproducible regression.

## What's still not here

* Real crun's own *other* call site (`StartTransientUnit` failure →
  reset-and-retry) — not added here: this project's own `create_scope`
  already tolerates a creation failure by falling back to "no cgroup at
  all" (logged, not fatal) rather than retrying, and a stale failed
  unit blocking a *retry* specifically isn't a scenario that fallback
  path can even reach. A future increment could still add a bare
  `reset_failed_unit` call in that fallback branch for extra
  thoroughness, but it wasn't judged to add real value without also
  adding the retry logic itself, which is out of this increment's own
  narrow scope.
* `ociman run -d`/`--detach`, `ocirun update`/`pause`/`resume`, the
  build cache, `ONBUILD`/`HEALTHCHECK` — all still exactly as earlier
  increments left them, unrelated to this increment's own scope.
