# Design note 0023: `ociman exec`

Status: implemented
Scope: `oci_runtime_core::launch::run_reporting_pid`, `ociman exec`.

## The gap this closed — and the prerequisite it actually needed

0022 built `ocirun exec` on top of `create`/`start`'s already-live,
backgrounded, pid-tracked containers. Naively adding `ociman exec` the
same way would not have worked at all: `ociman run` is still fully
foreground (`oci_runtime_core::launch::run` blocks until the container
exits and never exposes an intermediate pid to its caller), and 0021's
own "honest limitation" section already flagged exactly this — a
container record `ociman run` writes never has a *live* pid a
concurrent invocation could act on, only `None` throughout and finally
a `Stopped` record after the fact.

## `run_reporting_pid`: the same pid-pipe mechanism `create` already has, now shared with `run`

Rather than duplicate `create`'s pipe-based pid reporting a second
time, `launch::run` is now implemented in terms of a new
`run_reporting_pid(bundle, rootfs, on_pid)`, which forks (reporting the
real container pid back over a pipe, exactly like `create` does),
calls `on_pid` with it, and *then* waits for the container to actually
finish — combining `create`'s "report the pid early" behavior with
`run`'s own "block until done and return the exit code" behavior.
`run` itself is just `run_reporting_pid(bundle, rootfs, |_pid| {})`, so
every existing caller (`ocirun run`) is unaffected except for the cost
of one extra pipe and a 4-byte read — confirmed with the same
`hyperfine` methodology 0012/0018 already established: **3.0ms mean,
unchanged within noise from the pre-change ~2.9ms baseline**.

`ociman run`'s own `on_pid` callback records the container's real pid
and flips its state to `Running` *before* the container actually
finishes — so a separate `ociman ps`/`exec`/`rm` invocation, issued
while the first is still blocking in the foreground, now sees a real,
actionable pid rather than the `Creating` placeholder 0021 always left
in place until the process exited.

## `ociman exec` itself: a thin wrapper around the same primitive `ocirun exec` uses

Once a live pid is available, `ociman exec` is almost entirely CLI
glue: look the container up in `ociman`'s own container store, require
`effective_status() == Running`, read its bundle back for the same
`process.user`/`capabilities`/`no_new_privileges`/`cwd`/`env`/
namespace list `ocirun exec` (0022) already reads, and call the exact
same `oci_runtime_core::exec::exec` function. No new runtime-level code
was needed at all — the entire feature was `nsenter`/`exec` (0022)
plus `run_reporting_pid` (this note) plus about forty lines of glue.

## Verified against a real, concurrently-running container

Manually verified end to end (a real `docker.io/library/busybox` pull,
`ociman run ... sleep 30` backgrounded via a separate shell job, a
*second*, concurrent `ociman` invocation acting on it, deleted after):
`ociman ps` (no `-a`) correctly showed the container as `running` with
a real pid *while the first `ociman run` invocation was still blocked
in the foreground* — the exact scenario 0021 flagged as not working
yet. `ociman exec` into it printed the container's own hostname (its
short ID, matching `synthesize_spec`), `ps aux` showed both the
container's own init (`sleep 30`, pid 1) and the exec'd process at a
different pid, and the original container was completely unaffected
(still `running`, same pid) afterward.

## Real, automated, end-to-end tests

Two new tests in `tests/tests/ociman_exec.rs`, using the same fully
offline seeded-image approach 0020/0021 established. Unlike those
files' tests (which only ever call `.output()` and wait for `ociman
run` to finish), these `spawn()` a backgrounded `run` (stdio detached,
same reasoning `oci_tools_tests::ocirun_create` already documents for
`ocirun`'s own backgrounded containers) and poll `ociman ps` until the
container reaches `running` before attempting `exec` against it — the
first genuinely concurrent multi-process test scenario in this
project's own test suite, made possible specifically by this
increment's `run_reporting_pid` change.

## What's still not here

* `--user`/`--cwd`/`--env` overrides for `exec` (same gap 0022 already
  flagged for `ocirun exec`).
* `ociman logs` — still needs stdout/stderr redirected to a per-
  container log file for a container `ociman run` keeps running after
  its own invocation returns (nothing yet redirects a container's
  output anywhere other than terminal inheritance).
* A properly reflected `Running` status is now written mid-run, but the
  `Creating` window between `containers.create()` and the callback
  firing is still momentarily inaccurate (matches how `ocirun create`'s
  own `Creating` -> `Created` transition already has a similar brief
  window) — not a regression, just an inherent property of recording
  state from multiple steps of a single operation.
