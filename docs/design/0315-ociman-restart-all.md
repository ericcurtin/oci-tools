# Design note 0315: `ociman restart --all`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_start.rs`.

## Closing another slice of `0311`'s own "still ahead"

`0311` named `ociman kill`/`stop`/`restart --all`/`--cidfile`/
`--ignore` as real, separately-scoped candidates. `0312`/`0313` closed
`kill`/`stop --all`. This note closes `restart --all` — the last of
the three. `restart --cidfile`/`--ignore` remain future candidates.

## Real, checked-directly semantics — genuinely simpler than `kill`/`stop --all`

Read `~/git/podman/cmd/podman/containers/restart.go` and `pkg/domain/
infra/abi/containers.go`'s own `ContainerRestart`/`getContainers`
directly, then verified empirically against a real installed podman
with a real mix of running/never-started/already-stopped/paused
containers:

- `ContainerRestart` lists every container (`getContainers(all:
  options.All, ...)`) and, for each, calls `RestartWithTimeout`
  unconditionally — `libpod/container_internal.go`'s own
  `restartWithTimeout`: stop only if the container is actually
  `Running`, then re-`init`/start regardless of whatever state that
  left it in.
- Unlike `kill`/`stop --all`, there is **no skip category at all**:
  verified live with two running, one never-started (`create`d, never
  `start`ed), and one already-`Stopped` container — every single one
  is genuinely restarted (the never-started one is simply started for
  the first time; the already-stopped one is started again), each
  printing its own id, no error for any of them.
- The one real, reported failure is a genuinely `Paused` container —
  verified live: `podman restart --all` against a mix including one
  errors with `unable to restart a container in a paused or unknown
  state`, while every other container in the same call still restarts
  successfully. This matches `0313`'s own already-implemented
  single-target `restart` refusal for a paused container exactly (both
  go through the same underlying `stopInternal`/`stop_container`
  refusal in their respective codebases).

## Implementation

`Command::Restart`'s `id: String` became `id: Option<String>`, plus a
new `all: bool` (`--all`/`-a`), matching `Command::Kill`/`Command::
Stop`'s own identical shape (`0312`/`0313`). The existing single-
target restart body was factored out into a new `restart_one` helper
so the new `--all` loop can call it once per container, printing each
one's own id (via `cmd_start`'s own existing `launch_detached_and_
confirm(..., print_id: true, ...)`, unchanged) or accumulating the
first real failure while still attempting every remaining container —
matching `kill`/`stop --all`'s own identical "attempt every one,
report the first real failure at the end" shape. No skip condition at
all in the loop itself, matching the "every container genuinely
restarted" ground truth above.

## A real bug found and fixed before this ever shipped

Manually testing the very first version of this `--all` loop by hand
hit a genuine panic partway through a real, multi-container run: this
project's own debug-only single-threaded-at-`fork()` safety net
(`oci_runtime_core::process::fork`, `0160`) correctly caught a real
violation, not a false positive.

Root cause: `restart_one`'s own old-scope cleanup
(`reset_failed_systemd_scope`, `0159`) spawns a background D-Bus
thread that is *deliberately never joined* — a well-founded assumption
for every caller before this note, since the whole `ociman` process
always exited moments later regardless, making an occasionally-
abandoned thread free to leak. `--all` breaks that assumption outright:
the same process now goes on to call `fork()` again for the *next*
container in the loop, and a still-alive abandoned thread from the
*previous* container's own scope-reset call violates `fork`'s own
single-threaded-caller safety requirement for that next call —
reproduced directly (a real panic on the second or later container in
a `--all` run), not merely theorized.

Fixed by widening `restart_one`'s own already-established "defer this
background thread until no more forking can possibly happen in this
process" reasoning (previously scoped to just the rest of one restart
call) to cover the *entire* `--all` sweep: a new `defer_scope_reset`
parameter makes `restart_one` return what it would have reset instead
of spawning the thread itself; `cmd_restart`'s own `--all` loop collects
every one of these across every container, then performs them all,
sequentially, only *after* the entire loop (every container's own
`fork()` already long done) — at which point spawning that many
background threads back-to-back is exactly as safe as the original
single-target case always was.

## Verified

Manual, end-to-end, cross-checked directly against a real installed
podman: a mix of two running, one never-started, one already-stopped,
and one paused container — `restart --all` restarts all four
attemptable ones (a real, different pid for each previously-running
one, the never-started one transitioning to `stopped` having actually
run for the first time) and reports a real, immediate error only for
the paused one, leaving it completely untouched; `restart --all
some-id` (both given) is a clear, immediate "cannot give both" error;
existing single-target `restart` behavior (unchanged, verified via the
full pre-existing `ociman_start.rs` suite) is untouched. The `fork()`-
safety panic reproduced once with the initial, unfixed implementation
and did not recur after the fix, across several repeated manual runs.

Integration (`tests/tests/ociman_start.rs`, 3 new tests, 15 total, 12
pre-existing): `--all` restarts a running container (new pid), a
never-started one (started for the first time), all in one call;
`--all` combined with an explicit id is a clear error; a paused
container in the mix is a real, reported error while every other
container is still successfully restarted.

Regression: all 15 `ociman_start.rs` tests pass (12 pre-existing + 3
new); `ociman_stop.rs` (13, re-verifying `0313` is unaffected) and
`ociman_kill.rs` (7, re-verifying `0312` is unaffected) both still
pass unchanged. Full `cargo test --workspace --locked`: 112 test
result blocks, 0 failures (one known `ocicri_container.rs` flake under
full parallel load hit on the first `native-ci.sh` run this turn —
`exec_sync_runs_commands_in_a_running_container`, untouched by this
change — re-verified passing in isolation and on a clean full re-run).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).

Performance: `ociman restart` is a one-shot, offline command, not part
of any hot-path benchmark tracked in `docs/benchmarks.md`. The common,
no-`--all` case is entirely unchanged in cost (the deferred-scope-reset
plumbing only activates under `--all`, and even there costs nothing
extra — the same background threads that were always going to be
spawned are simply spawned a little later, sequentially, rather than
each immediately after its own container). No re-benchmark needed.

## Still ahead

`ociman restart --cidfile`/`--ignore` remain real, separately-scoped
candidates — with this note, `kill`/`stop`/`restart` all now support
`--all`, closing `0311`'s own three-part "still ahead" list in full.
The paused-container `SIGKILL`-delivery gap `0312` first found (and
`0313` worked around for `stop`/`restart` via an upfront refusal rather
than a real fix) remains its own real, separately-scoped, deliberately
deferred future candidate too.
