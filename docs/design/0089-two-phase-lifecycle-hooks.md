# Design note 0089: `prestart`/`createRuntime`/`poststart`/`poststop` for `create`/`start`/`delete` (milestone 3)

Status: implemented
Scope: `crates/oci-runtime-core/src/launch.rs` (`create`'s new
pre-pivot-hooks synchronization, `run_poststart_hooks`/
`run_poststop_hooks`), `bin/ocirun/src/main.rs` (`cmd_start`/
`cmd_delete`'s new hook calls), `tests/tests/ocirun_lifecycle.rs`.

All six real OCI runtime-spec hook points now run for **every**
lifecycle this project supports, not just `run`. 0026/0035/0088 wired
all six into `run_reporting_pid` (`ocirun run`/`ociman run`'s shared
combined create-and-start path); the separate `create`/`start`/`kill`/
`delete` two-phase lifecycle (`ocirun`-only; `ociman` doesn't expose
this split at all) still ran none of them at all until now.

## Verified against real runc source first, not assumed

Grepped `~/git/runc/libcontainer/` directly before writing any code:

* `process_linux.go`'s `procHooks` case — `prestart`/`createRuntime`
  run **synchronously inside `c.start()`**, which both
  `Container.Start` and `Container.Run` call and wait on before ever
  returning to their own caller. So `ocirun create` blocking on these
  two hooks before returning (this increment's own approach) matches
  real runc exactly — not a new deviation from 0035's original
  `run`-only scope, just extending the *same* already-correct timing
  to the second lifecycle that needed it.
* `container_linux.go`'s `Container.exec()` — signals the exec fifo,
  *then* calls `postStart()`. `Exec()` (real runc's own `start`
  subcommand) calls exactly this. So `ocirun start` running
  `poststart` right after `exec_fifo::signal_start` matches exactly.
* `state_linux.go`'s `destroy()` — always calls
  `runPoststopHooks(c)` as part of tearing a container down,
  regardless of what state it was in. So `ocirun delete` always
  running `poststop` (best-effort, matching this file's own existing
  `remove_cgroup_directory_if_any` tolerance for a since-moved bundle)
  matches exactly.

One deliberate, pre-existing divergence *not* changed by this
increment: real runc's `postStart()` kills the whole container if the
hook itself fails; this project's own `poststart`/`poststop` handling
(established back in 0026, for the `run` lifecycle) logs and tolerates
a failure instead, never changing the operation's own success/failure.
Kept identical for the two-phase lifecycle, for consistency rather
than introducing a second, harsher failure mode only for `create`/
`start`/`delete`.

## `create`: reusing `run_reporting_pid`'s own established synchronization pattern

`launch::create` gained the exact same `hooks_ready_write`/
`ChildSetup::hooks_ready_read` pipe dance `run_reporting_pid` already
has (see 0035): only set up when `bundle.spec.hooks` has a real
`prestart`/`createRuntime` entry, the container's forked child blocks
on it right before `pivot_root` (`ChildSetup::mount_pivot_and_exec`,
unchanged), `create` runs both hook lists in its own process once the
real container pid is known, then writes the unblock byte — or, on a
hook failure, kills the still-blocked child and returns the error
(same fatal handling `run_reporting_pid` already has). No refactor of
`run_reporting_pid` itself: the ~20 lines are a deliberate, small,
explicit duplicate (this project's own established preference — see
`ociboot list`'s own `boot_count_status` for the same reasoning)
rather than a bigger, riskier extraction that would touch the
already-correct, already-tested `run` path too.

## `start`/`delete`: two new public wrapper functions, reused from a re-loaded `Bundle`

`ocirun start`/`delete` are wholly separate CLI invocations from
`create` — no `Bundle` value survives between them, only
`state.bundle`'s own path string (persisted by `StateStore`). Both
now call `oci_runtime_core::Bundle::load(&state.bundle)` (tolerant of
failure, matching `remove_cgroup_directory_if_any`'s own established
precedent for exactly this same "bundle might have moved" case) and
then one of two new, small, public `oci_runtime_core::launch`
functions — `run_poststart_hooks`/`run_poststop_hooks` — both thin
wrappers around the already-existing, unchanged private
`run_lifecycle_hooks` helper `run_reporting_pid` itself already calls.
`state.pid` (already persisted by `create`) is threaded straight into
`run_poststart_hooks`.

## Real, manual verification against a real kernel first

Before writing any automated test: a full `create` → `start` →
`delete` cycle with all four hooks configured, confirming `prestart`
then `createRuntime` both finish before `create` itself returns
(order log); `start` correctly runs `poststart` with status
`"running"` and the real persisted pid, right after the container's
own command actually starts running; `delete` correctly runs
`poststop` with status `"stopped"`/`pid: 0`. Also verified a failing
`prestart` hook aborts `create` outright with no state left behind
(`ocirun state` correctly fails afterward) and no lingering container
process.

## Real, automated tests

Four new cases in `tests/tests/ocirun_lifecycle.rs`, following the
same patterns `ocirun_hooks.rs`'s own `run`-lifecycle tests already
established: `create` running `prestart` then `createRuntime` in
order before returning; a failing `prestart` hook aborting `create`
entirely with no state left behind; `start` running `poststart` with
the correct state and real pid; `delete` running `poststop` with the
correct state. All pre-existing lifecycle tests (create/start/kill/
delete, pid files, cgroup cleanup) still pass unchanged.

## Performance — hot-path change (`oci-runtime-core::launch`'s own `create`), A/B re-verified

`launch::create` is explicitly named in this project's own "always
re-verify" list (`oci-runtime-core` hot-path code). A `git stash`/
`git stash pop` A/B `hyperfine` comparison against a hookless bundle
(`ocirun create`, no hooks configured — the common case) showed the
new version 1.06× *faster* than before, well within this project's
own established noise band for zero-cost-when-unused changes. No
plausible regression mechanism: the added cost for a hookless bundle
is exactly one `is_some_and` check on an `Option` — no allocation, no
extra syscall.

## What's still not here

* `createContainer`/`startContainer` needed no extra work here at
  all — they already run unconditionally from inside
  `ChildSetup::mount_pivot_and_exec` regardless of which of `run`/
  `create` forked it (0088), so the two-phase lifecycle already had
  them.
* Automated failed-systemd-scope cleanup, `-v`/`--volume`'s own more
  advanced options, the build cache, `ONBUILD`/`HEALTHCHECK` — all
  still exactly as earlier increments left them, unrelated to this
  increment's own scope.
