# Design note 0432: `ocirun run --no-subreaper`

Status: implemented
Scope: `bin/ocirun/src/main.rs`, `tests/tests/ocirun_run.rs`,
`README.md`.

## What this closes

`ocirun run` had no `--no-subreaper` flag at all — this project's
own container-running process never set the Linux "subreaper"
attribute (`prctl(2)`'s `PR_SET_CHILD_SUBREAPER`) on itself, unlike
both real reference runtimes. Real `runc run --no-subreaper`/`crun
run --no-subreaper` both opt back *out* of a real default behavior
this project had never implemented in the first place — closing this
gap means implementing the underlying default *and* the opt-out flag
together, not just wiring a flag onto existing logic.

## Real, checked-directly confirmation

`~/git/runc/run.go:60-63` (flag, `run`/`restore` only — confirmed
absent from `create.go`'s own flag list). `~/git/runc/utils_linux.go:
264-267`:

```go
if r.enableSubreaper {
    // set us as the subreaper before registering the signal handler for the container
    if err := system.SetSubreaper(1); err != nil {
        logrus.Warn(err)
    }
}
```

right before registering the signal handler and blocking on the
container's own exit. `~/git/crun/src/libcrun/container.c`'s own
`libcrun_container_run_internal` does the identical real `prctl(
PR_SET_CHILD_SUBREAPER, 1, ...)` unless `LIBCRUN_RUN_OPTIONS_NO_
SUBREAPER`. Both confirmed: a failure to set the attribute is
**logged and tolerated**, never fatal.

Confirmed directly why `runc create` needs no equivalent flag at all
(not merely assumed): `create` returns immediately without ever
blocking on the container's own exit, so setting this attribute in a
process about to exit has no real, lasting effect to opt out of in
the first place — the exact same reasoning applies to `ocirun
create`.

## Implementation

- `Command::Run` gains `no_subreaper: bool` (`#[arg(long =
  "no-subreaper")]`). Deliberately not added to `Command::Create` at
  all (see above).
- The actual `prctl` call lives in the one function both of `ocirun
  run`'s own two call sites already share — `run_and_finalize`
  (called directly by the foreground path, and from inside the
  forked `--detach` keeper) — set right before the shared
  `launch::run_reporting_pid` call that actually blocks on the
  container's exit, matching real runc's own exact placement. A
  failure is logged via `tracing::warn!` and tolerated, matching
  real runc's own identical `logrus.Warn(err)` (never fatal).
- Uses `rustix::process::set_child_subreaper(Some(rustix::process::
  getpid()))` (already a workspace dependency with the `"process"`
  feature) — the real Linux syscall, not a placeholder.

## Tests

One new, fully end-to-end integration test in `tests/tests/
ocirun_run.rs`, `run_no_subreaper_stops_a_grandchild_orphan_from_
reparenting_to_ocirun`: runs a real container whose own init process
(an outer shell, sharing the *host* pid namespace — a real, separate
config, since a container with its own default, separate pid
namespace makes the whole effect unobservable from the host process
tree at all) forks an inner shell that backgrounds a long-lived
`sleep` and then exits, orphaning it while the outer shell (and
`ocirun` itself) are still alive. Reads the orphan's own real `PPid`
back from `/proc/<pid>/status` (the kernel's own ground truth, not a
guess) both with and without `--no-subreaper`, asserting the default
case reparents the orphan directly to `ocirun`'s own real pid, and
`--no-subreaper` does not. All 27 prior tests in `ocirun_run.rs`
continue to pass unmodified (28/28 total).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean, no diff), `cargo clippy --workspace --all-targets --locked --
-D warnings`, `cargo test --workspace --locked` (119 test-result
blocks, 0 failures, clean on the first full run), `python3 ci/
guards.py`, `cargo deny check`, `bash ci/native-ci.sh` (clean,
119/119), `bash ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg
-r` round trip). This touches the real container-run hot path (one
extra `prctl(2)` syscall per `run`), so `ci/bench.sh` was re-run:
`ocirun run` remains ~2.1× faster than `crun run` and ~6.4× faster
than `runc run`, consistent with historical results — no measurable
regression from one fixed-cost syscall.

## Deliberately still out of scope

`ocirun restore` doesn't exist in this project at all (checkpoint/
restore, already confirmed too big) — real runc's own `restore.go`
also registers this flag, with no equivalent here to extend.
