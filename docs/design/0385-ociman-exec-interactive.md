# Design note 0385: `ociman exec -i`/`--interactive`

Status: implemented
Scope: `crates/oci-runtime-core/src/exec.rs`, `bin/ociman/src/main.rs`,
`bin/ocirun/src/main.rs`, `bin/ocicri/src/launcher.rs`,
`tests/tests/ociman_exec.rs`, `tests/tests/ocirun_exec.rs`.

## What this closes

A real, previously-unnoticed correctness bug, not just a missing
convenience flag: `ociman exec` always forwarded whatever stdin its
own caller had, unconditionally, with no way to opt out — unlike real
`podman exec`'s own checked-directly default (`-i` absent) of never
connecting the exec'd process's stdin at all. An `ociman exec` invoked
from a script piping data on stdin (with no `-i` given) would leak
that data straight into the container process, where real podman would
not. `ociman run`/`create` already got this exactly right via
`launch.rs`'s `close_stdin` mechanism (0187) — `oci_runtime_core::exec`
never got the equivalent treatment.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/exec.go` (lines ~183-191):
  `AttachInput`/`InputStream` are only ever set when `-i`/
  `--interactive` is given; `AttachOutput`/`AttachError` (stdout/
  stderr) are unconditional either way. So `-i` gates *only* stdin —
  stdout/stderr must continue to be forwarded unconditionally
  regardless, both before and after this fix.
- **Purely a podman-level concept**: checked directly against both an
  installed `runc exec --help` and `crun exec --help` — neither has
  any `-i`/`--interactive` flag or concept at all. Both simply always
  forward whatever stdio their own caller gives them, unconditionally,
  by design — the same established asymmetry `ocirun run`/`create`
  already has relative to `ociman run`/`create` (0187). `ocirun exec`
  must not get a new flag; its `ExecRequest` construction hardcodes
  "always forward" instead.
- **The exact mechanism to port, corrected from an initial "pre_exec/
  dup2" assumption**: `launch.rs`'s own `close_stdin` does *not* use a
  `pre_exec`/`dup2` trick. It opens a fresh, host-side `/dev/null`
  *before* the fork (in the original process), stores it as
  `Option<OwnedFd>`, and — right before the final `command.exec()` —
  calls `command.stdin(unsafe { Stdio::from_raw_fd(fd.as_raw_fd()) })`.
  This must run *before* the unrelated `pre_exec` closure that closes
  fds `>= 3 + preserve_fds` (that cleanup would otherwise close the
  raw source fd behind `stdin_fd` before `Command` ever got a chance
  to `dup2` it onto fd 0).

## Implementation

- `oci_runtime_core::exec::ExecRequest` gains `pub close_stdin: bool`.
  `exec()` opens `/dev/null` (if requested) before the fork, in the
  original process — the same reasoning `nsenter::open_all` already
  uses for opening namespace fds before ever joining any of them.
  `ExecSetup` gains a matching `stdin_fd: Option<std::fs::File>`
  field, consumed in `exec_now()` with the identical `Stdio::
  from_raw_fd` reconstruction pattern `launch.rs` uses, registered
  before the existing fd-cleanup `pre_exec` closure.
- **Four call sites**, all updated:
  - `ociman`'s `cmd_exec` (the real fix): new `Command::Exec::
    interactive: bool` (`-i`/`--interactive`), `close_stdin:
    !interactive`.
  - `ociman`'s `cmd_healthcheck_run`: `close_stdin: true` — a
    healthcheck has no real interactive stdin to forward at all,
    matching real `podman healthcheck run`'s own identical lack of
    any attached stream.
  - `ocirun`'s `cmd_exec`: `close_stdin: false` — always forward,
    matching real `runc exec`/`crun exec` exactly, preserving today's
    existing behavior unchanged.
  - `ocicri`'s `__exec` launcher helper (`ExecSync`'s real
    implementation, 0240): `close_stdin: true` — real CRI's own
    `ExecSyncRequest` has no stdin concept at all (kubelet's own
    liveness/readiness probes never provide one), and this helper's
    own caller already captures stdout/stderr over real pipes
    regardless.

## Tests

Two new tests in `tests/tests/ociman_exec.rs`:
`exec_without_interactive_never_forwards_real_stdin` and
`exec_interactive_forwards_real_stdin` — mirroring `ociman_run.rs`'s
own already-established `run_without_interactive_never_forwards_real_
stdin`/`run_interactive_forwards_real_stdin` pattern exactly (a piped-
stdin child process, a busybox `read -t 5` probe, asserting `NOINPUT`
vs. the real forwarded line). One new regression-guard test in
`tests/tests/ocirun_exec.rs`, `exec_always_forwards_real_stdin_
unconditionally` — protecting the "no behavior change for `ocirun`"
invariant explicitly. All existing tests across `ociman_exec.rs`
(9 pre-existing), `ocirun_exec.rs` (12 pre-existing),
`ociman_healthcheck.rs` (5), and `ocicri_container.rs`'s own
`exec_sync_runs_commands_in_a_running_container` test continue to
pass unmodified.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures), `python3
ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh`, `bash
ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg -r` round trip).
This change touches the shared `oci_runtime_core::exec` module, a
directly `ci/bench.sh`-measured hot path for both `ocirun exec` and
`ociman exec` — targeted `hyperfine` re-runs: `ocirun exec` 1.9ms,
`ociman exec` 2.6ms, both matching the recorded baseline (1.9ms/2.8ms
respectively) within noise, no regression.
