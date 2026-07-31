# Design note 0387: `ocirun exec --pid-file`

Status: implemented
Scope: `crates/oci-runtime-core/src/exec.rs`, `bin/ocirun/src/main.rs`,
`tests/tests/ocirun_exec.rs`, `README.md`.

## What this closes

`ocirun run`/`create` already support `--pid-file` (writing the real
container pid to a file, matching real `runc create`/`run
--pid-file`), but `ocirun exec` had no equivalent — matching a real,
checked-directly gap relative to both real `runc exec --pid-file` and
`crun exec --pid-file`, each of which supports the exact same flag for
an *additional* process, not just a container's first one.

## Real, checked-directly confirmation

- `~/git/runc/exec.go` (line ~82): `pid-file` is a real, documented CLI
  flag on `runc exec`, read into `runner.pidFile` (`~/git/runc/
  utils_linux.go` line ~213).
- `~/git/runc/utils_linux.go` (lines ~310-314): `r.pidFile` is written
  via `createPidFile(r.pidFile, process)` right after the process
  starts (`tty.ClosePostStart()`), before ever forwarding signals or
  detaching — the exact same "as soon as the real pid is known, before
  blocking on the process's exit" timing `launch::run_reporting_pid`'s
  own `--pid-file` support (0170-era) already established for `ocirun
  run`/`create`.
- `~/git/runc/libcontainer/process_linux.go`'s `setnsProcess.
  execSetns`: when a PID namespace is joined, the pid handed to
  `createPidFile` is the *inner* forked child's pid — the outer relay
  (`PidFirstChild` there) is reaped as a zombie and discarded, never
  reported anywhere. The exact same distinction `launch.rs`'s own
  `ChildSetup::run` already draws between `grandchild_pid` (relay
  branch) and `own_pid` (no-relay branch).
- `~/git/crun/src/exec.c` (lines ~43, ~80, ~139, ~282): `crun exec
  --pid-file` is a real, equivalent flag (`crun_context.pid_file`),
  confirming this isn't a runc-only quirk.

## Implementation

- `oci_runtime_core::exec` gains `pub unsafe fn exec_reporting_pid(pid,
  request, on_pid: impl FnOnce(i32))` — the `exec` counterpart to
  `launch::run_reporting_pid`: opens the same real pid-reporting
  `CLOEXEC` pipe `launch::create` already uses, switches the
  underlying fork from `process::fork_and_wait` to a plain
  `process::fork` (so the direct child's pid is available to `wait` on
  separately, *after* reading the real pid back over the pipe), and
  calls `on_pid(exec_pid)` in the original (non-forked) process before
  that final wait. `exec()` itself is now a thin wrapper:
  `exec_reporting_pid(pid, request, |_pid| {})`.
- `ExecSetup` gains a `pid_pipe_write: OwnedFd` field (unconditional,
  since every `exec_reporting_pid` caller wants the real pid back) and
  a new `report_pid(&self, pid: i32)` method, mirroring
  `ChildSetup::report_container_pid` exactly (`rustix::io::write`,
  best-effort). `ExecSetup::run()` now calls it at exactly the same two
  points `ChildSetup::run` already does: the relay branch reports the
  *inner* fork's own pid (`child_pid` from the `process::fork(||
  self.exec_now())` call) before `wait_with_deadline`; the no-relay
  branch reports its own `rustix::process::getpid()` before calling
  `self.exec_now()`.
- A new private `read_exec_pid(read_fd) -> io::Result<i32>` function is
  an exact copy of `launch::read_container_pid`'s own 4-byte
  native-endian pipe protocol, kept as its own small copy rather than a
  cross-module `pub` export (matching this module's own existing
  `fail`-not-shared precedent).
- `bin/ocirun/src/main.rs`'s `Command::Exec` gains `pid_file:
  Option<PathBuf>` (`--pid-file`); `cmd_exec` now calls
  `exec_reporting_pid` (not `exec`) with a callback that reuses the
  existing `write_pid_file` helper `run`/`create --pid-file` already
  share.
- `ociman`/`ocicri`'s own three call sites are unaffected: they still
  call `exec()`, whose signature and behavior are unchanged.

## Tests

One new test in `tests/tests/ocirun_exec.rs`:
`exec_pid_file_writes_the_real_pid_of_the_exec_process` — spawns a
long-running `sleep 30` via `ocirun exec --pid-file`, waits for the
file to appear, reads the pid back, and proves it's the *real* pid of
the exec'd process itself (not this project's own outer relay pid) by
sending it a direct `SIGKILL` and asserting `ocirun exec`'s own exit
code becomes the matching `128 + SIGKILL` (`137`) — which could only
happen if the pid in the file really is the one this project's own
relay is blocked waiting on. The default bundle already joins a PID
namespace (see `exec_joins_the_running_containers_namespaces`'s own
doc comment), so this one test already exercises the PID-namespace-
relay branch — the only one worth its own dedicated coverage, since
the no-relay branch is just `rustix::process::getpid()` of the process
about to `exec` itself, with nothing else in between that could
plausibly report the wrong value. All existing tests across
`ocirun_exec.rs` (13 pre-existing), `ociman_exec.rs`,
`ociman_healthcheck.rs`, and `ocicri_container.rs`'s own
`exec_sync_runs_commands_in_a_running_container` continue to pass
unmodified (their call sites all still use the unchanged `exec()`
wrapper).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures; one
unrelated, pre-existing flake in `ociman_logs.rs`'s
`logs_follow_streams_a_running_containers_output_and_stops_when_it_
exits` under full parallel load, unrelated to `ocirun exec`/`exec.rs`,
confirmed by passing in isolation and on a full clean re-run), `python3
ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh`, `bash
ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg -r` round trip).
This change touches the shared `oci_runtime_core::exec` module, a
directly `ci/bench.sh`-measured hot path for both `ocirun exec` and
`ociman exec` — targeted `hyperfine` re-runs: `ocirun exec` 1.9ms,
`ociman exec` 2.8ms, both matching the recorded baseline exactly, no
regression.
