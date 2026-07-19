# Design note 0035: `prestart`/`createRuntime` hooks

Status: implemented (two more of the six real hook points; see below
for the remaining two)
Scope: `oci_runtime_core::launch` (`ChildSetup::hooks_ready_read`,
`run_pre_pivot_hooks`), wired into `run_reporting_pid`.

0026 shipped `poststart`/`poststop` and explicitly deferred the other
four hook points, `prestart`/`createRuntime` among them, for needing "a
synchronization point mid-way through the rootfs setup plan (between
'namespaces exist' and 'pivot_root about to run'), which `launch::run`'s
single fork-to-exec sequence has no such pause in today." This
increment adds exactly that pause.

## What the spec actually requires, checked against real `crun`

Per the real runtime-spec (`~/go/pkg/mod/github.com/opencontainers/
runtime-spec@v1.3.0/config.md`): `prestart`/`createRuntime` both run
"as part of the `create` operation, after the runtime environment has
been created ... but before `pivot_root` or any equivalent operation,"
in the **runtime namespace** (i.e. the runtime's own process, not the
container's) — `prestart` first, `createRuntime` second. Checked
against real `crun`'s own implementation
(`~/git/crun/src/libcrun/container.c`'s `libcrun_container_run_internal`,
around its own sync-socket protocol) rather than the spec prose alone:
`crun` runs both hook lists from its **own** process, in that exact
order, after the forked container process has reported its pid back
over a sync socket but while that same process is still paused waiting
for the runtime to write back — i.e. `crun` doesn't run these hooks
*inside* the container's own process or namespaces at all, despite the
container's namespaces already existing by this point; "runtime
namespace" really does mean the runtime's own process.

## The synchronization point: a second readiness pipe, same shape as 0034's

0034 (this same crate, same session) built exactly this kind of
pause-and-signal mechanism for the systemd cgroup driver: a pipe whose
read end the child blocks on at a specific point, unblocked by the
parent writing to it once some parent-side work (there: a D-Bus
migration; here: running hooks) is done. `ChildSetup` gained an
analogous `hooks_ready_read: Option<OwnedFd>` field, read (a blocking
one-byte read, `None` skipped entirely) as the *very first* thing
`ChildSetup::mount_pivot_and_exec` does — before even opening the exec
fifo, let alone running any mount from the rootfs plan or calling
`pivot_root`. This is the right place both semantically (namespaces
already exist by the time `mount_pivot_and_exec` runs — `unshare` and,
for the PID-namespace case, the relay fork have already happened) and
practically: it's the earliest point in this project's own code that
corresponds to "before `pivot_root`."

`run_reporting_pid` only creates this pipe at all when
`bundle.spec.hooks` actually has a `prestart` or `createRuntime` entry
— an ordinary container (every bundle this project's own benchmark has
ever used) pays nothing beyond one `Option` check on either side, the
same "only pay for what's configured" discipline `exec_fifo`/the log
tee pipe/0034's own cgroup-ready pipe already established.

Once `read_container_pid` confirms the container's own process has
reported its pid (which happens *before* it would ever reach the new
pause point — see `hooks_ready_read`'s own doc comment for why there's
no deadlock risk here, the same reasoning 0034 already worked out for
its own analogous pipe), `run_reporting_pid` calls the new
`run_pre_pivot_hooks`, then writes the "go" byte.

## Unlike `poststart`/`poststop`: a failing hook is fatal, matching real `crun`

0026's `run_lifecycle_hooks` logs and tolerates a failing
`poststart`/`poststop` hook — deliberately, since the container has
already started (or already exited) by the time either runs, and a
broken notify/cleanup script shouldn't retroactively change the
container's own exit code. `prestart`/`createRuntime` are different:
real `crun`'s own call sites `goto fail` on a nonzero hook result,
aborting container creation entirely — these two hook points exist
specifically so something (a CNI plugin, a mount-fixup script) can
*reject* a container before it ever runs, which silently continuing
past a failure would defeat entirely.

`run_pre_pivot_hooks` therefore returns a real `io::Result<()>`,
propagated as an error from `run_reporting_pid` itself. Since the
container's own process is, at the moment of failure, genuinely paused
waiting for a "go" byte that will now never come, `run_reporting_pid`
kills it outright (`process::kill(container_pid, SIGKILL)`) rather than
leaving it to hang forever, reaps the direct child (avoiding a
zombie), joins the log tee thread if one was spawned (its own read end
sees `EOF` once the killed child's stdout/stderr close, so this doesn't
hang either), and removes any cgroup directory that might already have
been created — the same cleanup `run_reporting_pid`'s own normal exit
path already does, just triggered earlier.

`prestart` running first, and skipping `createRuntime` entirely if
`prestart` itself fails, also matches `crun` exactly (`run_pre_pivot_
hooks` returns as soon as the `prestart` list fails, via `?`, before
ever calling `hooks::run` on `create_runtime`).

## Real, automated, end-to-end tests

`tests/tests/ocirun_hooks.rs` gained four new cases, following the same
pattern 0026's own `poststart`/`poststop` tests already established (a
real built `ocirun run`, a real busybox bundle, hooks injected as raw
JSON into `config.json`): a `prestart` hook receiving `status:
"created"` with a real, positive pid (not yet `"running"` — the
container hasn't executed anything yet yet at this point) and the
correct bundle path; a `createRuntime` hook receiving the same
`"created"` status; `prestart` provably finishing before `createRuntime`
starts (both append to the same host-side log file, checked for the
exact expected order, not just "both ran eventually"); and a failing
`prestart` hook both failing the whole `ocirun run` (nonzero exit) and
provably preventing `createRuntime` from ever running at all (checked
via a marker file `createRuntime` would have created).

Manually verified against a real kernel first, per this project's own
established discipline, before writing any of the automated tests
above (scratch bundle, deleted after): both hooks running in the
correct order with the correct state JSON on a real rootless
container using the default `pid` namespace (the pidns-relay-fork
code path, not just the simpler no-pidns one); a failing `prestart`
hook correctly aborting the container with no leftover process; and an
unmodified bundle with no `hooks` at all still running exactly as
before.

## Performance

Same "only pay for what's configured" reasoning as 0034's own cgroup
pipe: this increment's entire cost for a bundle without `prestart`/
`createRuntime` hooks (every bundle this project's own benchmark has
ever used) is one `bool`-producing `Option`/`is_empty` check on the
parent side and one `Option` check on the child side — no new pipe,
no new syscall. Re-confirmed with the same `hyperfine` methodology:
**3.0ms mean** for `ocirun run`, unchanged within noise from the
established ~2.6-3.1ms baseline.

## What's still not here

* `createContainer`/`startContainer` — both run in the **container's
  own namespaces**, not the runtime's, a different mechanism from this
  increment's synchronization-pipe approach: they need the hook process
  itself launched from *inside* the already-created namespaces (the
  forked child/grandchild, at the equivalent point in its own
  timeline), not merely a pause-and-signal handshake with the parent.
  A plausible, not-yet-implemented approach: run them from
  `ChildSetup::mount_pivot_and_exec` itself (`createContainer` at the
  same "before `pivot_root`" point this increment's pause sits at, just
  executed by the child rather than signaled to the parent;
  `startContainer` right before the final `exec`, matching the spec's
  "before the user-specified process is executed" wording exactly).
* `poststart`/`poststop`/`prestart`/`createRuntime` for the
  `create`/`start`/`kill`/`delete` two-phase lifecycle (0017) — same
  gap 0026 already flagged for `poststart`/`poststop`, now also true of
  these two: only `run` (used by both `ocirun run` and `ociman run`)
  gets any hooks executed at all.
