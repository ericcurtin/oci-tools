# Design note 0533: `ocirun exec --detach`/`-d`

Status: implemented
Scope: `crates/oci-runtime-core/src/exec.rs`, `bin/ocirun/src/main.rs`,
`bin/ocicri/src/launcher.rs`, `bin/ociman/src/main.rs`,
`tests/tests/ocirun_exec.rs`.

## What this closes

Real `runc exec --detach`/`-d` and `crun exec --detach`/`-d` both
start an additional process inside an already-running container and
return success immediately, instead of blocking until it exits.
`ocirun exec` had no equivalent at all — a real CLI flag it would
reject as unrecognized.

## Real, checked-directly confirmation

- `~/git/runc/exec.go:77-80`:
  ```go
  &cli.BoolFlag{
      Name:    "detach",
      Aliases: []string{"d"},
      Usage:   "detach from the container's process",
  },
  ```
- `~/git/runc/utils_linux.go`'s own `runner.run`: `detach := r.detach
  || (r.action == CT_ACT_CREATE)`, then, after starting the process
  and writing the pid file: `if detach { return 0, nil }` — detached
  `exec` returns as soon as the process is started/pid-filed, never
  blocking on exit.
- `~/git/crun/src/exec.c:76,162-163,280`: `{ "detach", 'd', 0, 0,
  "detach the command in the background", 0 }`, parsed into
  `exec_options.detach`, forwarded into `crun_context.detach`.
- `~/git/crun/src/libcrun/linux.c:6553-6554` (`libcrun_join_process`):
  `if (! detach) { ret = prctl(PR_SET_CHILD_SUBREAPER, 1, ...); ... }`
  — confirms crun's own detach mode deliberately skips becoming the
  exec'd process's own subreaper too; it's simply left to whichever
  ancestor already is one, or `PID 1`.

## Implementation

`crates/oci-runtime-core/src/exec.rs`:
- New `ExecRequest::detach: bool`. `exec_reporting_pid` captures it
  before `request`'s own fields are moved into `ExecSetup`, then,
  right after `on_pid(exec_pid)` (which already runs the `--pid-file`
  write, exactly matching real runc's own "write the pid file, then
  return" order with zero reordering needed), returns `Ok(0)`
  immediately when `detach` is set — never calling `process::wait
  (direct_child_pid)` at all. Unlike `ocirun run --detach` (0375), no
  background "keeper" process is needed here: `exec` has no
  persisted, queryable-afterward state of its own to maintain: simply
  not waiting and letting the kernel reparent the detached process
  (or, when a pid-namespace relay is involved, the whole relay chain)
  to the nearest subreaper/`PID 1` once this invocation itself exits
  is both correct and sufficient — the exact same real mechanism both
  reference runtimes' own detach modes rely on, confirmed above.
- Every other existing caller (`ocirun exec`'s own non-detached
  default, `ociman exec`/`healthcheck run`, `ocicri`'s own `ExecSync`
  launcher) now passes `detach: false` explicitly at its own
  `ExecRequest` literal, preserving today's exact blocking behavior
  byte-for-byte — a genuine `--detach` on `ociman exec` itself (real
  `podman exec -d` does have one, checked directly,
  `~/git/podman/cmd/podman/containers/exec.go:64`) is a real,
  separate, deliberately out-of-scope gap for a future increment, not
  closed here.

`bin/ocirun/src/main.rs`: new `Command::Exec::detach: bool`,
`#[arg(short = 'd', long)]`, threaded through `cmd_exec` into the new
`ExecRequest::detach` field.

## Tests

Three new integration tests in `tests/tests/ocirun_exec.rs`:
- `exec_detach_returns_immediately_without_waiting_for_the_command_to_finish`
  — a real wall-clock bound (`< 2s`) on the `exec --detach` call
  itself, given a `sleep 3`-then-marker-write command; confirms the
  marker doesn't exist yet right after the call returns, the
  container itself stays `running` throughout, and the marker
  eventually does appear once polled for.
- `exec_detach_still_writes_the_pid_file_before_returning` — composes
  `--detach` with `--pid-file`, confirming the file already exists
  (no poll needed) with a real, positive pid, by the time the fast
  call returns.
- `exec_detach_exits_zero_even_though_the_detached_command_will_eventually_fail`
  — `--detach`'s own exit code is always `0`, regardless of the
  detached command's own eventual (here, deliberately nonzero) exit.

A real, previously-unseen test-harness pitfall hit and fixed while
writing these (not a bug in the feature itself): the shared `ocirun`
test helper captures stdout/stderr via a real pipe through `Command::
output()`. A detached grandchild process inherits that same pipe, so
`output()` never sees `EOF` — and never returns — until *that*
process *also* exits, silently hiding the very behavior these tests
set out to prove (the first attempt at the timing test failed with an
elapsed time matching the *full*, un-detached sleep duration, which a
careful read of `exec_reporting_pid`'s own diff ruled out as the real
implementation's fault before looking anywhere else). Fixed by using
`Stdio::null()` for the detached invocation's own stdin/stdout/stderr
in all three tests — the exact same real hazard `ocirun_create`'s own
pre-existing doc comment in `tests/src/lib.rs` already documents for
an analogous case (a container left running in the background after
`create`).

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean after two auto-fixes), `cargo clippy --workspace
--all-targets --locked -- -D warnings` (clean), the full `ocirun_exec.rs`
suite (20/20, including every pre-existing test — confirming the new
`ExecRequest::detach` field didn't disturb the non-detached default
path at all), `ociman_exec.rs`/`ociman_healthcheck.rs`/`ocicri_container.rs`'s
own `ExecSync` test (all passing, confirming every other `ExecRequest`
call site updated correctly), a full `cargo test --workspace --locked`
run (125 test-result blocks, 0 failures, fully clean on the first
attempt), `python3 ci/guards.py` (clean), `cargo deny check` (clean),
`bash ci/native-ci.sh` (clean on the first attempt), `bash
ci/build-deb.sh` (clean on the first attempt, real `dpkg -i`/
`--version`/`dpkg -r` round trip). Since this change touches
`exec_reporting_pid`'s own shared plumbing (a real, exercised
`ci/bench.sh` benchmark), also re-ran `bash ci/bench.sh` in full: `ocirun
exec` still `1.68×` faster than `crun exec` and `8.38×` faster than
`runc exec` — no regression versus `0526`'s own last full
re-verification (`1.73×`/comparable), the small movement well within
ordinary run-to-run noise. Every other comparison in the same run
(`ocirun run`, `ociman exec`/`run -d`/`commit`/`build`/`rm`) likewise
shows no regression.

## Deliberately still out of scope

`ociman exec --detach`/`-d` (real `podman exec -d`) — a real,
separate gap, not closed here; this increment is scoped to the
lower-level `ocirun` runtime layer only, matching the exact candidate
this turn's own research targeted.
