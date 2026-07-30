# Design note 0375: `ocirun run --detach`/`-d`

Status: implemented
Scope: `bin/ocirun/Cargo.toml`, `bin/ocirun/src/main.rs`,
`tests/tests/ocirun_run.rs`, `README.md`.

## What this closes

The next natural follow-up `0373`/`0374` both left on the table: real
runc's/crun's own `run -d`/`--detach` — start the container and return
almost immediately instead of blocking in the foreground until it
exits.

## Real, checked-directly confirmation

`~/git/runc/utils_linux.go`'s own `runner.run` shares one
implementation between plain `run` and `-d` (and `create`, which is
internally `detach := r.detach || (r.action == CT_ACT_CREATE)`):
`detach` changes IO setup (moot here — `ocirun` has no TTY/console-
socket support at all yet) and skips the signal handler (moot here too
— `ocirun`'s existing foreground `run` already has no signal-
forwarding concept of its own). The one part that actually matters:
`if detach { return 0, nil }` — a detached run returns success as soon
as the pid is known, never blocking on `handler.forward` (the wait-
for-exit loop a foreground run performs). Cleanup
(`shouldDestroy`/`--keep`) is gated by `!detach || retErr != nil`: a
*successful* detach never calls `destroy()` at all — the state must
stay behind while the container is still running, `--keep` or not
(`--keep` only matters for the foreground/errored-detach case, which
still runs to completion).

`~/git/crun/src/libcrun/container.c`'s own `libcrun_container_run`
confirms the same shape from the other direction: without detach, it
runs synchronously in the calling process; with it, a real `fork()` +
`detach_process()` (`setsid()` then a *second* `fork()`, parent exits
— genuinely stronger hardening than a plain `setsid()` alone) creates
a real child that becomes the container's own direct parent, while the
original process returns as soon as it knows the fork succeeded.
`libcrun_container_create` always forces `context->detach = 1`
unconditionally — confirming `create` really is architecturally "`run`,
but forced-detach, plus an exec-fifo gate before the real command
runs", not a related-but-separate code path `run -d` could be built by
composing.

**This confirms `ocirun run -d` must not be implemented as "spawn
`create` then immediately `start`"** — that would reinvent the
create/start two-phase lifecycle (with its own separate exec-fifo
round-trip) for no reason, and would genuinely diverge from what real
runc's/crun's own `run -d` actually does (run the real command
immediately, no fifo gate at all).

## Implementation

Mirrors `ociman run -d`'s own already-shipped pattern (`docs/design/
0098`) almost exactly, ported to `ocirun`'s narrower feature set (no
`--rm`, no `--interactive`, no systemd cgroup driver):

- `Command::Run` gains `#[arg(short = 'd', long)] detach: bool`.
- `cmd_run`'s previous single-function body is split: the synchronous
  prefix (bundle load, `validate::validate`, `store.create`) stays
  unchanged either way — a bad bundle/config.json is still reported
  immediately, matching real runc (setup failures are never silently
  backgrounded). The former tail (the `run_reporting_pid` call, its
  pid-reporting callback, and the `if !keep { store.remove(id) }`
  finalization `0374` added) is extracted into a new, shared
  `run_and_finalize`, callable from two places — byte-identical logic
  either way, exactly like `ociman`'s own function of the same
  purpose and name.
- **Foreground path** (no `--detach`): calls `run_and_finalize`
  directly, then `std::process::exit(exit_code)`, unchanged from
  before this increment.
- **Detached path**: forks a keeper via `oci_runtime_core::process::
  fork` that calls `rustix::process::setsid()`, `dup2`s stdin/stdout/
  stderr to `/dev/null` (unlike `ociman`'s own keeper, `ocirun run` has
  no `--interactive` concept at all, so stdin is always silenced too,
  no conditional), re-opens a fresh `StateStore` handle, calls the same
  `run_and_finalize`, and exits `0` or [`oci_runtime_core::launch::
  SETUP_FAILURE_EXIT_CODE`] (125). The original invocation then calls a
  new `wait_for_detached_run_to_start` — polls the same persisted state
  until it leaves `Creating`, or (mirroring `ociman`'s own real,
  previously-hit 0189 race: an instantaneous container can run to
  completion and remove its own record before the very first poll)
  falls back to a real, blocking `waitpid` on the keeper's own pid,
  disambiguating "ran fine and already gone" (exit 0) from "genuinely
  failed to start" (125) — the exact same fallback `ociman`'s own
  `wait_for_detached_container_to_start` already uses.
- **A real, deliberate UX divergence from `ociman run -d`**: this
  invocation prints nothing at all on success, matching real `runc run
  -d`'s own checked-directly silence — `ociman run -d` prints the
  container id (Docker/podman convention), but `ocirun` bills itself as
  the lower-level, runc-CLI-compatible layer, so faithfulness to real
  runc's own behavior wins here rather than reflexively copying
  `ociman`'s own habit.
- `--pid-file`/`--keep` both continue to work unchanged, running inside
  the detached keeper instead of the original process — no special
  case needed for either.
- `bin/ocirun/Cargo.toml` gained `rustix = { workspace = true, features
  = ["process", "stdio"] }` (the same addition `ociman`'s own 0098
  needed for its own `setsid`/`dup2_std*` calls) — no new low-level
  primitive was needed in `oci-runtime-core` itself; `process::fork`/
  `wait`/`alive`/`exit_code_from_wait_status` (already public, already
  used exactly this way by `ociman`) covered everything.

One real, documented divergence: a plain `setsid()`, not crun's own
additional second `fork()` (which guarantees the detached process can
never reacquire a controlling terminal by becoming a session leader
again) — the same, simpler choice `ociman`'s own keeper already made
and has shipped fine in practice; noted honestly in the CLI's own doc
comment rather than silently assumed equivalent.

## Tests

Two new tests in `tests/tests/ocirun_run.rs`:
`run_detach_returns_immediately_with_the_container_still_running` (a
`sleep 30`'d container: the detaching invocation returns in well under
10 seconds, prints nothing to stdout, and a concurrent `ocirun state`
sees it genuinely `running`; killed, then confirms the state is fully
removed once it exits, matching the no-`--keep` default) and
`run_detach_keep_leaves_a_stopped_state_behind_for_a_later_delete`
(`--detach --keep` combined: the state survives as `stopped` once the
container exits on its own, and a later `ocirun delete` is still
needed to actually clean it up). All 20 tests in the file (18
pre-existing + 2 new) pass.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures on the clean
run; one incidental `ocicri_container.rs` flake under full parallel
load on the first attempt — a known, pre-existing flake, re-run in
isolation and confirmed unrelated, then the full suite re-run clean),
`python3 ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh`
(hit the identical known `ocicri_container.rs` flake once, same
re-run-and-confirm), `bash ci/build-deb.sh` (real `dpkg -i`/
`--version`/`dpkg -r` round trip). This change touches `ocirun run`, a
directly `ci/bench.sh`-measured hot path (the foreground, non-detached
path is unchanged code, just moved into a separate function) — re-run
specifically: 3.2ms mean, unchanged from the `0374` baseline (3.0ms)
within noise.
