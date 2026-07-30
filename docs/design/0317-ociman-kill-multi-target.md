# Design note 0317: `ociman kill` multi-target ids

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_kill.rs`.

## Closing another slice of `0316`'s own "still ahead"

`0316` named `ociman kill`/`stop` as still single-target-only (plus
`--all`), a real, separately-scoped gap since real podman's own `kill`/
`stop` both accept `CONTAINER [CONTAINER...]`. This note closes it for
`kill`. `stop`'s own identical multi-target gap, plus its own real
`--cidfile`/`--ignore` (never implemented for `stop` at all — `0313`
only ever closed `--all` for it), remain future candidates.

## Real, checked-directly semantics

Read `~/git/podman/cmd/podman/containers/kill.go`'s own `Use: "kill
[options] CONTAINER [CONTAINER...]"` and `pkg/domain/infra/abi/
containers.go`'s own `getContainers` `default` case directly (the
exact same two-phase behavior `0316` already established for
`restart`'s own multi-target support): every given name is resolved
via `LookupContainer` first, and any resolution failure aborts the
*whole* function immediately, before ever attempting to kill even the
ones that did resolve. Once every given id has resolved, though, the
actual kill loop (`ContainerKill`) still attempts every one of them
regardless of an earlier one's own signal-send failure, accumulating
each one's own error to report at the end.

One more real, checked-directly detail this note specifically needed
(not an issue for `restart`, since its own single-target path never
prints the raw argument at all — `0316`'s own test-authoring
correction): the CLI-level printing rule. Real podman's own dispatch
(`cmd/podman/containers/kill.go`) prints `r.RawInput` (the original
string given) if set, only falling back to `r.Id` (the resolved
canonical id) otherwise. This project's own existing single-target
`cmd_kill` already matched that (`println!("{id}")` using the original
argument, not a resolved one) — the multi-target path preserves this
exactly, printing each raw name/id given, not the resolved one.

## Implementation

`Command::Kill`'s `id: Option<String>` became `ids: Vec<String>`,
matching `Command::Restart`'s own identical widening (`0316`). The
existing single-target kill logic was factored into a new `kill_one`
helper (`containers`, `resolved` id, `raw_id` for printing, `sig`) so
both the single- and multi-target paths share it:

- Zero targets, no `--all`: unchanged clear error.
- `--all`: entirely unchanged from `0312`.
- Exactly one target, no `--all`: the original, simplest possible
  path, unchanged in cost or behavior.
- Two or more explicit targets (a real, new capability): every one is
  resolved first via `resolve_container_id`, aborting the whole call
  immediately if any fails — matching real podman's own two-phase
  behavior exactly. Only once every one has resolved does the loop
  actually attempt each `kill_one`, accumulating the first real
  failure while still attempting every remaining target.

Unlike `restart`'s own identical-shaped widening (`0316`), no deferred-
scope-reset handling was needed here at all: `kill` never forks a new
process (it only ever sends a signal), so there is no `fork()`-safety
concern for a loop of several kills in the same process the way there
was for a loop of several restarts.

## Verified

Manual, end-to-end: `ociman kill run1 run2` signals both and prints
each one's own raw given name (not a resolved id); `ociman kill run3
nonexistent-xyz` aborts the whole call, leaving `run3` genuinely
untouched (confirmed still `running` afterward) rather than killing it
before failing on the second name; `ociman kill run3 run1` (one
running, one already-stopped) still attempts both, printing `run3` and
reporting a real error for `run1`, matching the existing single-target
`kill`'s own "not running is a hard error" rule exactly.

Integration (`tests/tests/ociman_kill.rs`, 2 new tests, 9 total, 7
pre-existing): multiple explicit ids each get signaled and each one's
own raw name is printed (verified via a sorted-lines comparison, since
signal delivery/print ordering across two independently-forked
containers isn't itself a guarantee this note needs to make); an
unresolvable id among several aborts the whole call before signaling
any of them, leaving the real container completely untouched.

Regression: all 9 `ociman_kill.rs` tests pass (7 pre-existing + 2
new); full `cargo test --workspace --locked`: 112 test result blocks,
0 failures.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).

Performance: `ociman kill` is a one-shot, offline command, not part of
any hot-path benchmark tracked in `docs/benchmarks.md`. The
overwhelmingly common single-target case is provably unchanged in cost
(still the exact original code path via `kill_one`, no new branching
or allocation before it). No re-benchmark needed.

## Still ahead

`ociman stop` remains single-target-only (plus `--all`), and never got
its own real `--cidfile`/`--ignore` at all (unlike `rm`, and unlike
`restart`'s own new `--cidfile` from `0316`) — real podman's `stop`
has both. The paused-container `SIGKILL`-delivery gap `0312` first
found remains its own real, separately-scoped, deliberately deferred
future candidate too.
