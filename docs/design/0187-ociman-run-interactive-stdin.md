# Design note 0187: `ociman run -i`/`--interactive`, and closing a
matching stdin-forwarding bug in `ociman build`'s `RUN` steps

Status: implemented
Scope: `crates/oci-runtime-core/src/launch.rs` (`ChildSetup::stdin_fd`,
`run`/`run_reporting_pid`'s new `close_stdin` parameter);
`bin/ocirun/src/main.rs` (`run_reporting_pid` call site); `bin/ociman/
src/main.rs` (`Command::Run`'s new `--interactive` flag, `cmd_run`,
`run_and_finalize`, `launch_detached_and_confirm`,
`attach_and_wait_for_exit` race fix); `bin/ociman/src/build.rs` (`RUN`
step execution); `tests/tests/ociman_run.rs`, `tests/tests/
ociman_build.rs`.

## A real, previously-unnoticed bug, found by hand before writing any
code

Before touching anything, checked directly what real `docker`/`podman`
actually do with a container's own stdin by default:

```
echo hi | podman run --rm busybox sh -c 'read -t 2 line && echo GOT:$line || echo NOINPUT'
# -> NOINPUT
echo hi | podman run --rm -i busybox sh -c 'read -t 2 line && echo GOT:$line || echo NOINPUT'
# -> GOT:hi
```

Then checked `ociman run`'s own actual behavior the same way (via a
`Command::spawn` with real piped stdin, not `.output()`, which closes
stdin by default and would have hidden this): `ociman run --rm
busybox ...` (no flag at all) forwarded real host stdin
unconditionally, every time — because `oci_runtime_core::launch`
never touched the container's own stdin (fd 0) at all, so it just
inherited whatever fd 0 the calling `ociman` process itself had. A
real, checked-directly deviation from both real tools, not a
hypothesis: this project had no way to *not* forward stdin, and no
`-i` flag to opt into forwarding it deliberately either.

The exact same root cause affects `ociman build`'s `RUN` step
execution too (`bin/ociman/src/build.rs`, via the same `oci_runtime_
core::launch::run`): checked directly that real `podman build`'s own
`RUN` steps never see real stdin at all, even when the `build`
invocation itself had some piped in — `ociman build`'s `RUN` steps did,
before this fix.

## Scope: only `ociman` needs a new parameter; `ocirun` must not change

`ocirun run`/`create` (this project's own `runc run`/`crun run`
equivalent) has no `-i`/`--interactive` flag at all, matching real
`runc`/`crun` exactly — checked directly, neither has any "attach"/
"interactive" concept at that layer: they always just forward whatever
stdio their own caller (`podman`/`docker`, or here `ociman`) already
set up. So the fix threads a new `close_stdin: bool` parameter through
`oci_runtime_core::launch::run`/`run_reporting_pid` (implemented via a
new `ChildSetup::stdin_fd: Option<OwnedFd>`, mirroring the existing
`stdout_log_fd`/`stderr_log_fd` pattern: `Some` overrides, `None`
inherits, unchanged from before this field existed), with each call
site choosing explicitly:

* `ocirun run`'s own direct `run_reporting_pid` call: `false` — must
  never change, matching real runc/crun exactly.
* `ociman build`'s `RUN` step (`launch::run`): `true`, always — no
  flag of its own, matching real `docker build`/`podman build`'s own
  unconditional behavior (verified directly, see above).
* `ociman run`: `!interactive`, threaded from a new `-i`/`--interactive`
  flag on `Command::Run` (default `false`, i.e. `close_stdin: true` —
  matching real `docker run`/`podman run`'s own default exactly). Has
  no effect with `--detach` (a detached container's own stdin is
  always closed either way, via the existing keeper-process `setsid`+
  `/dev/null` redirection from 0098 — real docker/podman's own
  separate `-d -i` "leave stdin open for a later attach" behavior is a
  separate, still-deferred gap, not attempted here).
* `ociman start`/`ociman create`: no `-i` of its own yet (still-
  deferred, matches `cmd_start`'s own doc comment already naming this
  gap from 0186) — both pass `false` explicitly.

## A second, real, previously-latent bug found and fixed along the way

While manually verifying the fix end to end, `ociman start --attach`
(0186) started intermittently reporting the wrong exit code (`-1`
instead of the container's own real one) — reproduced reliably by
hand (`ociman create` + `ociman start --attach` in a loop), not merely
suspected. Root cause, found by reading the raw persisted `state.json`
directly (a genuine `strace`/`gdb` is unavailable in this sandbox,
`ptrace_scope` blocks both): `attach_and_wait_for_exit`'s own polling
loop checked `state.effective_status() == Status::Stopped`, but
`effective_status()` deliberately reports `Stopped` the instant the
container's own recorded pid is no longer alive — which can be
*before* its own detached keeper process has actually gotten around to
persisting the real final state (`ANNOTATION_EXIT_CODE` included, both
written together in one call at the very end of `run_and_finalize`).
This is the exact same race `wait_for_keeper_to_finalize`'s own doc
comment already documents and guards against elsewhere (`cmd_start`'s
own initial precondition check, `stop_container`'s own early-return
case) by checking the *raw* `state.status` field instead — `attach_
and_wait_for_exit` (new in 0186) simply never applied the same fix,
since 0186's own manual verification pass happened to never lose that
race in practice. This turn's stdin change (one extra `/dev/null`
open per launch) shifted the timing just enough to expose it
reliably. Fixed the same way: check the raw `state.status` field, not
`effective_status()`.

## Tests

Two new integration tests in `tests/tests/ociman_run.rs`
(`run_without_interactive_never_forwards_real_stdin`,
`run_interactive_forwards_real_stdin`), one new integration test in
`tests/tests/ociman_build.rs` (`build_run_step_never_sees_real_host_
stdin`) — all three spawn the real binary with a real piped stdin
(`Command::spawn`, not `.output()`, which would have silently masked
the very bug being tested for). Verified the `attach_and_wait_for_exit`
race fix by hand first (5 repeated `create`+`start --attach` cycles,
previously failing intermittently, now consistently correct), then
confirmed `start_attach_streams_output_and_propagates_exit_code`
(0186) passes reliably again (3 repeated `cargo test` runs). Full
`cargo build --workspace --locked`/`cargo test --workspace --locked`
(2 clean runs across the whole workspace, 83/83 result blocks, 0
failures)/`cargo fmt --all --check`/`cargo clippy --workspace
--all-targets --locked -- -D warnings`/`python3 ci/guards.py`/`cargo
deny check`/`bash ci/native-ci.sh` all clean. One transient, unrelated
hang was observed in one `cargo test --workspace` run, isolated to
`oci-runtime-core`'s own `overlay::tests::the_scratch_directory_is_
always_removed_regardless_of_the_result` — that module's own doc
comment already explicitly documents this exact class of hazard
(calling a `fork()`-based probe from an already-multi-threaded test
binary can, rarely, leave a lock a different thread held at fork time
stuck forever in the child); reproduced zero times in 5 repeated
isolated runs of that same test, and the very next full `cargo test
--workspace` run completed cleanly — a known, pre-existing,
undocumented-until-now-as-actually-observed flake in a module
unrelated to this change, not a regression from it.

## A quick, targeted performance sanity check

Since this touches every single container launch's own hot path
(`oci_runtime_core::launch`), re-benchmarked directly before
finishing: `ocirun run` (bundle-only, no image/state overhead) ~3.0ms
(previously measured ~3.3-3.4ms, within noise, no regression); `ociman
run --rm` ~66ms vs a real `podman run --rm`'s own ~199ms on the same
host, same image (~3× faster, consistent with previously measured
~2.8-4.3× figures across this project's own history) — the one new
`/dev/null` open plus one new `Option` check per launch costs nothing
measurable, the same conclusion 0169/0184's own analogous additions
already reached for their own comparably-sized per-launch additions.

## What this doesn't do yet

`-i` for `ociman start`/`ociman create` (a separate, still-deferred
gap, unchanged from 0186); `-d -i`'s own real "leave stdin open for a
later attach" behavior for `ociman run` (this project has no
`attach`-to-an-already-running-container command at all yet, only
`start --attach`, which only ever applies to an already-`Stopped`/
`Created` container); `--interactive`'s own real terminal/pty
allocation (`-t`/`--tty`) is a wholly separate, unstarted gap.
