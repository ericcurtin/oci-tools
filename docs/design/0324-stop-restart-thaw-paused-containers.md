# Design note 0324: `stop`/`restart` genuinely succeed against a paused container

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_stop.rs`,
`tests/tests/ociman_start.rs`.

## Closing the single most-repeated "still ahead" item this session

Six consecutive design notes (`0313`, `0315`, `0316`, `0317`, `0318`,
`0319`, `0320`) each named the same real, deliberately-deferred gap:
`ociman stop`/`restart` hard-refuse a genuinely paused container
outright, matching real podman's own identical refusal there, rather
than actually making the stop/restart happen the way `kill` now
correctly does (`0319`). This note finally closes it.

## Why this isn't merely "matching real podman" — a genuine improvement

Real podman's own `stop`/`restart` don't solve this either — they
refuse. Checked directly, `libpod/container_internal.go`'s own
`stopInternal`: its own allowed-to-attempt state set is `Created`/
`Running`/`Stopping`, deliberately excluding `Paused` — it never even
attempts a signal against a paused container, refusing immediately
with `ErrCtrStateInvalid` instead (verified live: both real `podman
stop`/`podman restart` on a paused container are a real, immediate
error — `restart`'s own error is even more explicit: "unable to
restart a container in a paused or unknown state").

This project's own previous behavior *before* this note matched that
refusal (`0313`). This note deliberately goes further: `stop`/
`restart` now genuinely thaw a paused container as part of delivering
its first signal and then proceed exactly as they would for any other
running container — no `unpause` step required first. This is a real
improvement over both real reference tools, not merely parity with
either, continuing the exact same "beat the equivalent" precedent
`0319` already established for `kill`.

## Implementation

The fix is small and surgical, entirely inside the one function both
`stop` and `restart` already share (`stop_container` — `cmd_restart`'s
own `restart_one` calls it unchanged, so this fix applies to `restart`
too with zero `restart`-specific code): the upfront `anyhow::ensure!
(display_status(&state) != Status::Paused, ...)` refusal is removed
entirely, and all three real signal-send call sites within the
function (the initial send, the early "signal handler might not be
installed yet" resend loop, and the final `KILL` escalation) now go
through `oci_runtime_core::cgroups::kill_thawing_if_paused` — the
exact same real, already-tested primitive `kill` itself has used since
`0319` — instead of a plain `process::kill`. The container's own
cgroup is thawed the moment the very first signal is actually,
successfully sent; every subsequent send in the same call is a
harmless no-op thaw-check against an already-thawed cgroup.

Before this fix, attempting the signal-then-escalate dance against a
still-frozen cgroup would have silently hung for the *entire* grace-
plus-escalation window (a signal a frozen cgroup's own freezer only
ever queues, never delivers, until thawed) and then falsely reported
success — this is the exact silent-false-success bug `0312` first
discovered for `kill`, now closed for `stop`/`restart` too, not merely
a cosmetic improvement over real podman's own equally real refusal.

## Verified

Manual, end-to-end, with a real release binary: `ociman stop --time 1`
on a still-paused container (no `unpause` first) now genuinely
transitions it to `stopped` in ~1.9s (the real grace-period timing
taking effect, not an instant no-op) — confirmed directly, before this
fix, that real `podman stop`/`podman restart` both still just error
immediately in the identical scenario.

Integration: `tests/tests/ociman_stop.rs`'s own former
`stop_and_restart_on_a_paused_container_are_a_real_immediate_error`
(asserting the old, now-superseded refusal) replaced with two tests
matching the corrected behavior — `stop_on_a_paused_container_
genuinely_thaws_and_stops_it` and `restart_on_a_paused_container_
genuinely_thaws_and_restarts_it` (the latter also confirming a real,
different pid afterward, not merely a status-string check).
`tests/tests/ociman_start.rs`'s own former `restart_all_reports_a_
real_error_for_a_paused_container_but_still_restarts_the_rest`
similarly replaced with `restart_all_genuinely_restarts_a_paused_
container_too`, confirming `--all` now genuinely restarts a paused
container in the mix (a real, different pid afterward) rather than
reporting an error for it.

Regression: all 20 `ociman_stop.rs` tests and all 19 `ociman_start.rs`
tests pass; `ociman_kill.rs` (10) and `ociman_pause.rs` (8) are
unaffected (neither test file exercised `stop`/`restart` against a
paused container at all). Full `cargo test --workspace --locked`: 113
test result blocks, 0 failures.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).

Performance: `stop`/`restart` are one-shot, offline commands, not part
of any hot-path benchmark tracked in `docs/benchmarks.md`. The common,
non-paused case is provably unchanged in cost (the same `kill_thawing_
if_paused` primitive already does nothing extra beyond one already-
established, harmless cgroup-freezer check when the target isn't
actually frozen, `0319`'s own already-verified sanity check). The
paused case is now dramatically *more useful* (a real stop/restart
succeeding, at real grace-period cost) rather than a fast refusal — a
strict functional improvement, not a regression, for the one case
whose behavior changed at all. No re-benchmark needed.

## Still ahead

With this note, every one of this session's own six-consecutive-notes
"still ahead" chain (`0313`→`0320`) is now fully closed: every
container-lifecycle command (`kill`/`stop`/`restart`/`rm`/`pause`/
`unpause`) both supports the full real podman `--all`/multi-target
combination it's supposed to, and (`kill`/`stop`/`restart`) genuinely
works against a paused container rather than refusing or silently
failing. `ocibox`'s own remaining gaps (icon handling for `export
--app`, `stop`/`upgrade`/`generate-entry`/`assemble`) and `ocivmm`'s
own remaining gaps (a lighter-weight offline `create` success-path
fixture, the HVF/macOS phase-4 blocker) remain separately-scoped
future candidates, as does `ociman rm --force`'s own similar-but-
distinct signal-to-a-paused-container consideration (spotted while
implementing this note: `remove_container`'s own SIGKILL-before-
removal step also predates this project's thaw-aware primitive, though
its own consequence is milder — the container's own files are removed
regardless of whether the underlying process ever actually died — a
real, separately-scoped candidate of its own, not touched here to keep
this note narrowly scoped to `stop`/`restart`).
