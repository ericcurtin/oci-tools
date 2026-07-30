# Design note 0355: `ociman rm -t`/`--time`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_ps.rs`.

## What this closes

Flagged in the same survey that found `0354`'s `kill --cidfile` gap:
real `podman rm -t`/`--time` had no equivalent in this project at all.

## Real, checked-directly semantics — and a real tension worth naming

Read `~/git/podman/cmd/podman/containers/rm.go` directly: `-t`/
`--time` requires `--force` too (`if cmd.Flag("time").Changed { if
!rmOptions.Force { return errors.New(...) } }`, real podman's own
exact text: *"--force option must be specified to use the --time
option"*). Traced the actual removal logic
(`~/git/podman/libpod/runtime_ctr.go`'s own `removeContainer`): for a
still-`Running`/`Stopping` container, it calls `c.stop(time)` — a
**real, graceful stop** (the container's own real stop signal, waited
on up to `time` seconds, only escalating to `KILL` if still alive
after that) — never a bare, immediate `KILL`.

The real surprise, found while reading this rather than assumed: real
podman's own `removeContainer` reaches this exact same graceful-
stop-then-kill call **even with a bare `--force` and no `--time` at
all** — `time := c.StopTimeout()` (the container's own resolved
default, 10 seconds unless overridden) is the fallback when
`opts.Timeout` is `nil`. In other words, real `podman rm --force`
*alone*, today, on a real installed podman, still waits up to 10 real
seconds for a graceful exit before escalating — it is not the
instant-`KILL` operation its own name might suggest.

This project's own `remove_container` (`0021`'s original
implementation) has never worked that way: `--force` on a running
container has always sent an immediate, bare `KILL` with no signal
escalation and no grace period at all. Faithfully reproducing real
podman's own slower default here would be a genuine, measurable
*regression* against this project's own explicitly stated goal —
*"beat their equivalents on all the benchmarks, especially startup
time and destroy time"* — for the single most common invocation
(`rm -f`, no `--time`) of one of the most benchmark-relevant commands
in the whole project (`ci/bench.sh`'s own dedicated `rm
(destroy-only...)` section exists specifically to keep this fast).

## Design decision: port the flag exactly, keep the fast default

`-t`/`--time`'s own real, checked-directly behavior (a genuine
opt-in escalation window, requiring `--force`) is ported exactly.
But the *default* (`--force` alone, `--time` never given) deliberately
stays this project's own already-established fast path — an
immediate `KILL`, zero grace period — rather than adopting real
podman's own slower one. This is the same kind of deliberate,
verified-directly, explicitly-documented divergence this project has
made before when a real upstream default conflicts with one of its
own stated pillars (rather than silently drifting, or blindly copying
upstream regardless of the project's own explicit goals).

## Implementation

New `Command::Rm::time: Option<u64>` (`-t`/`--time`); `cmd_rm` checks
`time_secs.is_none() || force` first, matching real podman's own
check ordering and exact error text. `remove_container` gained a
`time_secs: Option<u64>` parameter and now branches on it explicitly:

- `Some(secs)`: calls [`stop_container`] (the exact same primitive
  `ociman stop`/`restart` already use and have already had tested)
  with that timeout — a real, graceful escalation, composed rather
  than reimplemented.
- `None` (unchanged from before this note): the existing, fast,
  immediate-`KILL`-then-poll-for-death loop, untouched.

The `rmi --force`'s own dependent-container-removal cascade (a
pre-existing, unrelated caller of `remove_container`) passes `None`
explicitly — real `podman rmi` has no `--time` equivalent of its own
either, so this cascade correctly keeps using this project's own fast
default, matching real `podman rmi`'s own identical lack of any
grace-period concept for its cascade.

## Verified

New tests in `ociman_ps.rs`: `rm_time_without_force_is_a_clear_error`;
`rm_force_time_lets_a_signal_handling_container_exit_gracefully` (a
real TERM-trapping container, `rm --force --time 60`, proving the new
opt-in path genuinely lets the trap run *and* that the container's
own record ends up fully removed afterward — distinct from `ociman
stop`'s own identical escalation, which only stops it — same real
technique `ociman_stop.rs`'s own established graceful-exit test
already uses);
`rm_force_without_time_completes_fast_with_no_grace_period_at_all` (a
real running container, `rm --force` with no `--time` at all,
asserting real wall-clock completion well under any plausible grace
window — the regression guard for the "deliberate divergence" design
decision above, not just documentation of intent).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (113 test-result blocks,
0 failures — including all 39 pre-existing `ociman_ps.rs`/`rm`-related
tests and all 21 `ociman_rmi.rs` tests, unmodified, confirming the
default path is untouched), `python3 ci/guards.py`, `cargo deny
check`. `ci/bench.sh`'s own dedicated `rm` benchmark section
deliberately not re-run: it measures removing an already-*stopped*
container, a code path this note's own change doesn't touch at all
(neither the old nor the new branch is ever reached when `status ==
Stopped`) — the new `rm_force_without_time_completes_fast_with_no_
grace_period_at_all` test is the real, targeted regression guard for
the one code path that did change.
