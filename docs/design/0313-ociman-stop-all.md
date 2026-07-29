# Design note 0313: `ociman stop --all`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_stop.rs`.

## Closing another slice of `0311`'s own "still ahead"

`0311` named `ociman kill`/`stop`/`restart --all`/`--cidfile`/
`--ignore` as real, separately-scoped candidates. `0312` closed `kill
--all`. This note closes `stop --all`. `restart --all`/`--cidfile`/
`--ignore` remain future candidates.

## Real, checked-directly semantics — not assumed, and not identical to `kill --all`

Read `~/git/podman/cmd/podman/containers/stop.go` and `pkg/domain/
infra/abi/containers.go`'s own `containerStopImpl`/`ContainerRestart`
directly, then verified against a real installed podman with a real
mix of running/paused/never-started containers (not assumed to be
symmetric with `0312`'s own `kill --all`, which turned out to matter):

- `containerStopImpl` lists *every* container (`getContainers(all:
  options.All, ...)`), same shape as `kill --all`.
- Its own tolerant switch on a per-container stop failure: an already-
  `Stopped`/`Exited` container (`ErrCtrStopped`) is *always* silently
  tolerated, `--all` or not — this project's own `stop` already had
  this exact no-op before this note (`cmd_stop`'s own pre-existing doc
  comment). `ErrCtrStateInvalid` is *also* always tolerated by that
  same switch regardless of `--all` (a real, easily-missed quirk in
  podman's own source: the `--all`-gated case and the unconditional
  one below it swallow the identical error, the only difference being
  which log message fires).
- **Verified live, though, that this does not mean "everything is
  silently fine"**: real `podman stop --all` against a mix of two
  running, one never-started (`create`d, never `start`ed), and one
  genuinely `Paused` container — the two running ones stop normally;
  the never-started one is printed with **no error at all** (`stop`,
  unlike `kill`, considers `Created` a legitimately stoppable state,
  matching `libpod/container_internal.go`'s own `stopInternal`
  allowing `Created`/`Running`/`Stopping`); the **paused one is a real,
  reported error**, `--all` or not (`is running or paused, refusing to
  clean up: container state improper`, exit `125`) — the overall
  command's own exit code is non-zero, and the paused container is
  left completely untouched. This is a genuine, checked-directly
  *difference* from `kill --all`'s own tolerant treatment of `Paused`
  (0312): `stopInternal`'s own allowed-state set deliberately excludes
  `Paused` outright (`ensureState(Created, Running, Stopping)`), so it
  never even attempts a signal against one; a later, unconditional
  `Cleanup()` call then finds the container still genuinely alive and
  refuses, which is what actually surfaces to the user.
- `ContainerRestart` (used for `restart`, out of this note's own
  scope) has an identical real ground-truth refusal for a paused
  target (`unable to restart a container in a paused or unknown
  state`), confirmed live too.

## Implementation

`Command::Stop`'s `id: String` became `id: Option<String>`, plus a new
`all: bool` (`--all`/`-a`), matching `Command::Kill`'s own identical
shape (0312). `cmd_stop` now refuses both an explicit id and `--all`
together, and under `--all` iterates every container, skipping
(silent `continue`) only an already-`Stopped` one — every other
container (`Creating`/`Created`/`Running`/`Paused`) is attempted via
the existing `stop_container`, printing its id on success or
accumulating the first real failure while still attempting every
remaining one, exactly matching `kill --all`'s own "attempt every one,
report the first real failure at the end" shape.

Getting `stop --all` right required two real, pre-existing bugs in the
shared `stop_container` (used by both single-target `stop` and
`restart`, so both benefit) fixed alongside it — neither is new, both
predate this note entirely (confirmed directly: reproduced against
`a7d2c8c`, `0312`'s own last-pushed commit, before any change here),
but deciding what `--all` should silently tolerate vs. genuinely
attempt vs. hard-error on directly surfaced both:

1. **A container with no recorded pid at all** (`ociman create`'s own
   deliberately lazy design never pre-forks a real process the way
   real crun/runc's own two-phase `create`/`start` does — `Status::
   Created`'s own doc comment describes *that* model, not this
   project's) previously hit a confusing `"has no recorded pid"` hard
   error from `stop`/`restart`, rather than the same tolerant no-op
   real `podman stop --all` gives an identical never-started
   container (prints the id, no error, still `Created` afterward).
   Folded into the existing `Stopped`-is-a-no-op early return.
2. **A genuinely `Paused` container** previously had `stop`/`restart`
   blindly attempt the normal signal-then-escalate dance against it,
   which — since a frozen cgroup *queues* rather than delivers a
   signal until thawed (`docs/design/0312`'s own discovered, at-the-
   time-deferred gap) — silently hung for the *entire* grace-plus-
   escalation window and then reported a **false success**, the
   container never actually having stopped at all. Fixed with an
   upfront guard (reusing `display_status`, the same real, computed-
   from-the-live-cgroup-freezer check `ps`/`inspect` already use,
   since `effective_status()` itself never reports `Paused` at all —
   it is deliberately never *persisted*, see `Status::Paused`'s own
   doc comment) that now refuses immediately with a clear error,
   matching real podman's own actual refusal (verified: real
   `podman stop`/`restart` on a paused container are *also* a real,
   reported error, not a silent success) — just faster and with a
   clearer message than real podman's own confusing after-the-fact
   `Cleanup()` failure.

`cmd_restart` needed one more real fix directly caused by (2): it
strips `ANNOTATION_AUTO_REMOVE` before calling `stop_container` (so the
internal stop doesn't trigger auto-removal) and re-inserts it
afterward — but the re-insert was only reachable *after* a successful
stop, via `?`-propagation. Once `stop_container` can now genuinely
fail (the new `Paused` refusal), a failed `restart` on a `--rm`
container would have silently and permanently dropped its own
auto-remove-on-exit behavior. Fixed by capturing the stop result
first, always restoring the annotation, and only then propagating any
stop failure.

## Verified

Manual, end-to-end: a real mix of two running, one never-started, one
paused container — `stop --all` stops the two running ones, prints
the never-started one's id with no error, and reports a real,
immediate (not hung) error for the paused one while the other three
are still fully processed; a second `stop --all` call still correctly
re-attempts (and re-tolerates) the never-started one and still errors
only on the still-paused one; after `unpause`, a plain `stop` on it
succeeds normally. `stop --all some-id` (both given) is a clear,
immediate "cannot give both" error. `stop`/`restart` on a paused
container now fail in ~4ms instead of hanging ~9-18s before falsely
reporting success (measured directly, before and after).

Integration (`tests/tests/ociman_stop.rs`, 4 new tests, 13 total, 9
pre-existing): `--all` stops every running container and tolerates a
never-started one; `--all` combined with an explicit id is a clear
error; `--all` with no containers at all is a successful no-op; `stop`
and `restart` on a paused container are both a real, immediate error
(not a silent hang-then-false-success), leaving it genuinely untouched
and still stoppable normally once unpaused.

Regression: all 13 `ociman_stop.rs` tests pass (9 pre-existing + 4
new); `ociman_start.rs` (12, covering `restart`), `ociman_pause.rs`
(3), and `ociman_kill.rs` (7, re-verifying `0312` is unaffected) all
still pass unchanged. Full `cargo test --workspace --locked`: 112 test
result blocks, 0 failures.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).

Performance: `ociman stop`/`restart` are one-shot, offline commands,
not part of any hot-path benchmark tracked in `docs/benchmarks.md`.
The common, no-`--all`, non-paused case is unchanged in cost; the
paused case is now dramatically *faster* to fail (milliseconds instead
of the full grace-plus-escalation window), a strict improvement, not a
regression, for the one case whose cost changed at all. No
re-benchmark needed.

## Still ahead

`ociman restart --all`/`--cidfile`/`--ignore` remain real, separately-
scoped candidates. This project still has no way to actually *stop* a
genuinely paused container in one step (real podman doesn't either —
it requires an explicit `unpause` first, exactly matching what this
note now also requires) — teaching `stop` to thaw-then-signal a paused
container automatically, closing the underlying gap `0312` first
found, remains a real, separately-scoped, deliberately deferred
future candidate of its own.
