# Design note 0159: the real `ociman restart` scope-delay bug — root cause and fix

Status: implemented
Scope: `bin/ociman/src/main.rs` (new `ANNOTATION_SCOPE_NONCE` + new
`scope_name_for` helper; `run_and_finalize` generates a fresh nonce per
launch; `reset_failed_systemd_scope` takes `&PersistedState` instead of
just an id; `stop_container` gains a `reset_scope: bool` parameter;
`cmd_restart` defers its own old-scope cleanup until after `cmd_start`
has already forked the new keeper); `tests/tests/ociman_run.rs` (4
existing tests updated for the new scope-naming scheme, not new
coverage); `tests/tests/ociman_start.rs` (existing regression test
tightened with a real, tight timing bound).

## Closing 0158's own deferred gap — and finding the real root cause was something else entirely

0158's own "what this doesn't do yet" named a real, measured
performance issue: restarting a container made its own next stop take
several real seconds to finalize, hypothesized at the time to be
systemd's own transient-unit-name-reuse timing. This increment set out
to fix that by giving every real launch a fresh, unique scope name
(`ociman-<id>-<nonce>.scope`, [`ANNOTATION_SCOPE_NONCE`]) instead of
reusing the plain `ociman-<id>.scope` across every relaunch of the same
container.

**That fix alone made the measured delay *worse*, not better** (~2s
before, ~12s after) — a real, direct signal the original hypothesis was
wrong. Re-investigating with the scope name now provably unique (ruling
out name-reuse as a factor at all) led to the actual root cause:
`stop_container`'s own `reset_failed_systemd_scope` call spawns a
background *thread* of its own (`oci_runtime_core::systemd_cgroup::
reset_failed_unit`'s D-Bus round trip, deliberately never joined —
see its own doc comment: "if the timeout fires, this thread is simply
abandoned"). `cmd_restart` calls `stop_container` — which can spawn
this thread — and then, in the very same process, calls `cmd_start`,
which forks a brand new keeper (`launch_detached_and_confirm`'s own
`process::fork`). If that background thread was still alive at the
exact moment of that `fork()`, the calling process was not actually
single-threaded — a real, direct violation of `process::fork`'s own
documented safety contract ("the calling process must not have spawned
any additional threads by this point").

## Confirmed directly, not just theorized

Isolated with a targeted, temporary diagnostic (removing just the
`reset_failed_systemd_scope` calls inside `stop_container` and
re-measuring): the delay vanished entirely, and `systemctl --user
list-units` showed the new scope appearing and becoming `active
running` almost immediately — versus the *unmodified* code, where
`systemctl --user list-units` showed **zero** `ociman-*` units for the
entire multi-second window, meaning the new keeper's own `create_scope`
D-Bus call was never actually succeeding at all during that time,
consistent with a corrupted D-Bus/async-runtime state inherited across
a `fork()` from a parent that wasn't really single-threaded.

## The fix: defer the old scope's cleanup past the new keeper's own fork, not scope naming

Rather than trying to make `reset_failed_unit`'s own spawned thread
somehow safe to fork past (it can't be made to unconditionally block
without reintroducing the unbounded-hang risk its own timeout exists to
prevent), `stop_container` gained a `reset_scope: bool` parameter:
`cmd_stop` still passes `true` (nothing forks afterward in that path,
so spawning the thread there is harmless, same as always). `cmd_restart`
now passes `false` — deferring the *old* launch's own best-effort
"failed scope" cleanup — and only performs that reset itself
*after* its own `cmd_start` call has already forked the new keeper, at
which point `cmd_restart`'s own process never forks again, so a
background thread spawned at that point can no longer corrupt anything.

The unique-per-launch scope nonce ([`ANNOTATION_SCOPE_NONCE`]) is kept
regardless — it's still a real, independent improvement (no launch
ever depends on systemd's own unit-name-reuse timing/behavior at all,
whatever that behavior turns out to be on a given system), it's what
made the real root cause observable/provable in the first place, and
`reset_failed_systemd_scope`'s own now-correct targeting (via the new
`scope_name_for` helper, falling back to the plain nonce-less name for
state predating this annotation) depends on it to reset the right
scope for a `--force`-killed container or the deferred `cmd_restart`
cleanup above.

## Real, measured result

`tests/tests/ociman_start.rs`'s whole suite (7 tests) dropped from
17–27s to 2.7–12.7s across repeated runs. The specific regression test
for the original `--rm`/`restart` bug (0158) now asserts a **tight**
3-second bound for the post-restart real stop to finalize (previously
needed a 20-second window to avoid flaking on the multi-second stall)
— a real, direct guard against this exact bug reappearing, not just
against the end state being eventually correct.

## Real, automated tests

Four existing tests in `tests/tests/ociman_run.rs` (checking real
systemd unit properties like `CPUQuotaPerSecUSec`/`AllowedCPUs`) were
updated to read the container's own actual current scope name from its
real, persisted `state.json` (a new `real_scope_name` test helper)
rather than hardcoding the old, now-incomplete `ociman-<id>.scope`
pattern — not new coverage, a necessary adjustment for the new naming
scheme. `restart_does_not_auto_remove_a_rm_container_but_a_later_
real_stop_still_does` (`tests/tests/ociman_start.rs`, 0158) was
tightened as described above.

Full `cargo build --workspace --locked`/`cargo test --workspace
--locked` (2 clean runs)/`cargo fmt --all --check`/`cargo clippy
--workspace --all-targets --locked -- -D warnings`/`python3
ci/guards.py`/`cargo deny check`/`bash ci/native-ci.sh` all clean.

## What this doesn't do yet

The same general class of bug (spawning a background thread, then
`fork()`-ing in the same process without ever joining it) could in
principle recur anywhere a future increment adds a new D-Bus-backed
helper upstream of a `launch_detached_and_confirm`/`process::fork` call
site — this fix addresses the one concrete, confirmed instance
(`cmd_restart`), not a structural guarantee against every possible
future recurrence of the same pattern. A more systemic safeguard (e.g.
a debug-assertion helper that checks `/proc/self/task` is a single
entry immediately before every `process::fork` call) would be a
reasonable, separately-scoped future hardening measure.
