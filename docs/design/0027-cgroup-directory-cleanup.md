# Design note 0027: cgroup directory cleanup on container exit

Status: implemented
Scope: `oci_runtime_core::cgroups::remove`, wired into
`launch::run_reporting_pid` and `ocirun`'s own `cmd_delete`.

## The bug: every container leaked one empty cgroup directory, forever

0015 (cgroup directory creation + migration) shipped without any
cleanup step at all, on the belief — recorded in its own "what's still
not here" section — that the kernel would remove an empty cgroup on
its own. That belief doesn't hold up: the real kernel's own docs
(`~/git/linux/Documentation/admin-guide/cgroup-v2.rst`) describe an
empty cgroup as "considered empty and **can be removed**: `rmdir
$CGROUP_NAME`" — removal is presented as something the caller still
has to do, not something that happens automatically. 0015's own
"verified informally" note (the directory it manually created was
"already gone" by the time it went looking) was almost certainly
`systemd-run --user --scope` cleaning up *its own* transient scope's
cgroup once the scope ended — a `systemd` behavior, not a kernel one —
which doesn't apply to a `cgroupsPath` this project computed and
migrated into directly, outside of any systemd unit's own lifecycle.

The practical effect: every container ever run with a `cgroupsPath`
set (the realistic default shape for anything a real `podman`/`crun`
generates — see 0018's own benchmark note) left one empty, orphaned
cgroup directory behind under `/sys/fs/cgroup`, permanently, for as
long as the host stayed up. Not a crash or a functional failure, but a
real, unbounded resource leak directly contradicting this project's own
"beat the equivalents" goal (real `crun`/`runc` do clean this up).

## `cgroups::remove`: tolerant of "already gone" and of a brief race

`std::fs::remove_dir`, but:
* `NotFound` is `Ok(())` — nothing to clean up isn't a failure (a
  caller that never actually got as far as creating one, or one
  invoked twice, shouldn't have to special-case this itself).
* `ResourceBusy`/`DirectoryNotEmpty` are retried (bounded, 50 attempts
  with a 20ms sleep between): the kernel can take a brief moment after
  the last process in a cgroup actually exits before `rmdir` stops
  seeing it as populated — the exact same reasoning `ocirun delete`'s
  own kill-then-poll loop already documents elsewhere in this codebase
  for a different, analogous race.

## Wiring

* `launch::run_reporting_pid` (used by both `ocirun run` and `ociman
  run`) calls `remove_cgroup_directory_if_any` right after
  `process::wait` returns and the log-tee thread (if any) has been
  joined — the bundle it already has loaded gives it `cgroupsPath`
  directly, no extra I/O needed beyond the `rmdir` itself.
* `ocirun`'s own `cmd_delete` (the `create`/`start`/`kill`/`delete`
  two-phase lifecycle, 0017) needed a bit more: unlike `run`, `delete`
  is a wholly separate invocation from whichever `create` originally
  set the cgroup up, with only `state.bundle`'s path on hand — so it
  re-`Bundle::load`s that path for the one field (`linux.cgroupsPath`)
  it actually needs.

Both call sites log (`tracing::warn!`) and otherwise ignore a failure
here, matching the same "don't fail the container/deletion over
cleanup" reasoning already established for the log-tee thread (0025)
and lifecycle hooks (0026): a cgroup that fails to `rmdir` for some
unexpected reason must not block deleting the container's own state,
and must not turn a successful container run into a reported failure.

## Real, automated, end-to-end tests

Extended `run_creates_and_enters_the_requested_cgroup`
(`tests/tests/ocirun_run.rs`, real `systemd-run --user --scope` carrier,
same as 0015/0018 already established) with an assertion that the
cgroup directory no longer exists once `run` returns — this is the
exact real scenario the bug affected, now checked automatically instead
of only by manual inspection.

New `create_start_kill_delete_removes_the_cgroup_directory`
(`tests/tests/ocirun_lifecycle.rs`): the full real `create`/`start`/
`kill`/`delete` sequence, `create` wrapped in the same `systemd-run`
carrier (only `create` actually needs it — `start`/`kill`/`delete`
don't touch cgroups themselves, and `delete`'s own `rmdir` only needs
ordinary write access to the parent directory, which any process
running as this uid already has under a fully-delegated subtree),
asserting the cgroup directory exists after `create` and is gone after
`delete`.

`cgroups.rs`'s own unit tests (4 new cases): deleting an existing empty
directory, tolerating an already-missing one, and a real race — a
background thread removes the one file blocking `rmdir` partway through
`remove`'s own retry loop, proving the retry (not just the happy path)
actually works.

## Performance

`remove_cgroup_directory_if_any` is one `cgroups::directory_for` call
(a cheap path computation, `None` whenever `cgroupsPath` isn't set,
which is every bundle this project's own benchmark uses) followed by,
only when that's `Some`, one `rmdir` syscall — re-confirmed with the
same `hyperfine` methodology already established: **~3ms mean** (this
session's host had a second, unrelated concurrent process running
throughout, adding more session-to-session variance than usual — the
*relative* comparison stayed consistent: ocirun 3.7ms vs. a
freshly-remeasured `crun run` at 10.1ms, ~2.7x faster, matching every
prior session's own relative gap within noise).

## What's still not here

* The systemd cgroup driver itself (0015's own remaining gap, unrelated
  to this increment — this only fixes cleanup of the cgroupfs-driver
  directories this project already creates).
* Devices cgroup / eBPF device filtering (0015's other remaining gap).
* No cleanup for a container that's still `Created` but never
  `start`ed and then force-deleted mid-migration (an edge case: the
  existing `create`/`delete` tests already cover a never-started
  container's ordinary process cleanup; this increment's own cgroup
  test uses the full `start`-then-`kill`-then-`delete` path, not that
  specific edge case, though `remove`'s own "tolerant of NotFound"
  behavior means it wouldn't error either way).
