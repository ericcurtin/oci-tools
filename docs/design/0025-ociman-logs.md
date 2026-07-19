# Design note 0025: `ociman logs`

Status: implemented
Scope: `oci_runtime_core::launch` (`run_reporting_pid`, `ChildSetup`),
`ociman run`/`ociman logs`.

## The gap

Milestone 3's own scope (`ociman run/exec/ps/logs`) had one command
left: `logs`. `ociman run` never redirected a container's stdout/stderr
anywhere other than straight terminal inheritance, so there was nowhere
for a separate `ociman logs <id>` invocation — issued after the
container finished, or from another process entirely while it's still
running — to read anything back from.

Real `crun`/`runc` don't solve this either: the OCI runtime-spec has no
concept of log capture at all, and real `podman`/`docker` solve it with
a separate long-lived companion process (`conmon`) that holds the
container's pty/pipe open and writes a log file for as long as the
container runs, independent of whether the CLI invocation that started
it is even still alive. This project has no such companion process
yet, so the design had to fit within what a single, still-alive
`ociman run` invocation can do on its own.

## Design: a combined-stream tee, not a separate log-shim process

Since `ociman run` remains fully foreground (the same process that
started the container blocks in `process::wait` for its entire
lifetime — see 0021/0023, no `--detach` exists yet), the *same*
process can just also be the thing that captures the log: fork the
container exactly as before, but wire its stdout and stderr to a pipe
instead of leaving them to inherit this process's own, and have a
background thread in *this* process copy everything read from that
pipe to both a log file and this process's own real stdout — so a
foreground `ociman run` still shows live output exactly as it did
before this existed, while also durably persisting a copy for `ociman
logs` (or a second, concurrent `ociman run`/`exec`/`ps` invocation, the
same "still foreground but not alone" pattern 0021/0023 already
established) to read.

`oci_runtime_core::launch::run_reporting_pid` gained a `log_path:
Option<&Path>` parameter. `run` (unchanged for `ocirun`) is just
`run_reporting_pid(bundle, rootfs, None, |_pid| {})` — `None` skips
every bit of the new machinery below entirely, so `ocirun run`'s own
hot path (the one benchmarked against `crun`/`runc`) is unaffected.

## A real deadlock, found and fixed before this ever worked

The very first version of this stored the pipe's two write ends (one
for stdout, one — a `dup`'d copy — for stderr) as plain `RawFd`s on
`ChildSetup`, obtained via `OwnedFd::into_raw_fd()` in the *parent*
process before forking. This hung every single test that exercised it:
`into_raw_fd` deliberately does **not** close anything — it only
suppresses Rust's own `Drop` so the raw number can be handed to the
child. Since nothing else in the parent process ever closed those two
numbers either, the parent (`ociman run`'s own process) kept its own
copies of the pipe's write end open for its entire remaining lifetime —
including the exact lifetime the reader thread needed to see EOF on
the read end. A pipe's read side only ever sees EOF once *every* open
copy of the write side, across every process, is closed; as long as the
parent held one open itself, `read()` in the tee thread blocked
forever, even after the container had fully exited — a process
deadlocking itself on its own leftover fd.

The fix: store them as genuine `OwnedFd`s instead (`stdout_log_fd`/
`stderr_log_fd` on `ChildSetup`, exactly mirroring the already-working
`pid_pipe_write`). Moving the whole `ChildSetup` into the closure
passed to `process::fork` means the *parent's* own copies get dropped
completely normally when `fork`'s own stack frame returns in the
parent branch (the closure argument is simply never called there) —
the same mechanism that already made `pid_pipe_write` work correctly,
just not originally applied to these two new fields. The child side
(`mount_pivot_and_exec`, which only ever sees `&self`) reconstructs a
fresh `Stdio` via `unsafe { Stdio::from_raw_fd(fd.as_raw_fd()) }` at the
point of use rather than trying to move the `OwnedFd` out of a shared
reference — sound specifically because this leaf process always
terminates from there by either a successful `exec` (replacing the
process image, which reclaims every fd via ordinary kernel process
teardown) or `fail`'s `std::process::exit` (same reclaiming), so the
original field's own `Drop` never gets a chance to run either way —
there is exactly one thing that ever disposes of the fd, regardless of
which path is taken.

## Fork-safety: the reader thread has to wait until *after* the fork

The tee's reader thread cannot be spawned before `process::fork` is
called: doing so would make the calling process multi-threaded at
exactly the moment it forks, which every fork in this codebase (see
`process::fork`'s own safety contract) requires not to be true.
`run_reporting_pid` therefore only creates the pipe (via
`setup_log_tee_pipe`) before forking, and spawns the actual
`spawn_log_tee_thread` reader thread afterward, once back in the
parent.

A relay process (the pid-namespace case's own second, internal fork —
see 0012/0017) retains its own OS-level copies of the write ends for as
long as it itself is alive, which is bounded by (and ends immediately
after) waiting for its own grandchild — a benign, sub-millisecond delay
in EOF detection, not a correctness issue, matching the reasoning
already applied elsewhere in this codebase to similar relay-fork
side effects.

## `ociman logs`

Reads `<container-dir>/container.log` (written by the tee thread above)
and writes it verbatim to stdout. A container that exists but has no
log file yet (containers created before this feature existed) prints
nothing rather than erroring; only an unknown container ID is an error,
using the same `containers.load` every other subcommand already relies
on for that.

## Real, automated, end-to-end tests

`tests/tests/ociman_logs.rs`, using the same seeded-image approach
`ociman_run.rs`/`ociman_exec.rs` established:
* `logs_shows_a_finished_containers_combined_output` — a plain,
  already-`.output()`-awaited `ociman run` writing to both stdout and
  stderr, `logs` afterward showing both.
* `logs_shows_output_so_far_from_a_still_running_container` — the same
  `spawn()`+detached-stdio+poll pattern `ociman_exec.rs` uses for a
  genuinely concurrent scenario: `logs` against a container still
  `sleep`ing shows its pre-sleep output but not its post-sleep output
  yet, then shows both once it actually finishes.
* `logs_rejects_an_unknown_container_id`.

Manually verified against a real `docker.io/library/busybox` pull too:
a plain foreground `ociman run` echoing to the terminal exactly as
before, `ociman logs` afterward reproducing both its stdout and stderr
lines in the order they were produced; a backgrounded `ociman run
... sleep 2` with a concurrent `ociman logs` showing only its pre-sleep
line while still running and both lines after it finished.

## Performance

`ocirun run` is unaffected (`log_path` is always `None` for it, and
`run_reporting_pid`'s only addition when it's `None` is one `.map(...)
.transpose()` call that's immediately `None` — no pipe, no thread, no
extra syscalls) — reconfirmed with the same `hyperfine` methodology
0012/0018/0023 already established (100+ samples, 5 warmup runs, the
same rootless busybox bundle, `/bin/true`): **3.1ms mean**, unchanged
within noise from the ~2.9-3.0ms baseline, still roughly 3x faster than
a freshly re-measured `crun run` (9.4ms) on this same session's host.

## What's still not here

* `-f`/`--follow` — `logs` only ever prints what's been captured so
  far and exits, matching real `podman logs`/`docker logs`'s own
  *default* (non-`-f`) behavior, not their `-f` mode.
* stdout and stderr are combined into one stream, in the order bytes
  happened to arrive at the shared pipe, with no per-line record of
  which produced which (real docker's json-file log driver tracks
  this per entry). Doing so would need two separate pipes, each with
  its own reader thread and its own record of origin per chunk written
  to the log file, plus a log format that isn't just raw concatenated
  bytes — a bigger change than this increment's scope.
* No log rotation/size limits — `container.log` grows unbounded for a
  long-running or chatty container, same as an already-known,
  documented direction for a future increment (real docker/podman both
  eventually rotate).
* Terminal (`process.terminal = true`, pty-allocated) containers aren't
  a concern yet either way — nothing in this codebase requests one yet
  (`ociman`'s own `synthesize_spec` always sets `terminal = false`).
