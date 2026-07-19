# Design note 0088: `createContainer`/`startContainer` hooks (milestone 3)

Status: implemented
Scope: `crates/oci-runtime-core/src/launch.rs` (`ContainerHooks`,
`ChildSetup::container_hooks`/`run_container_hooks`,
`build_child_setup`'s new `id` parameter, `mount_pivot_and_exec`'s two
new call sites), `crates/oci-runtime-core/src/hooks.rs` (doc comment
only), `crates/oci-spec-types/src/runtime.rs` (doc comments only —
these two hook points were already fully parsed), `bin/ocirun/src/
main.rs` (`launch::create`'s new `id` argument, doc comment),
`tests/tests/ocirun_hooks.rs`.

The last two of the OCI runtime-spec's six real lifecycle hook points,
finally executed. `prestart`/`createRuntime`/`poststart`/`poststop`
have run since 0026/0035; `createContainer`/`startContainer` were
parsed (round-tripped through `config.json`) but silently ignored at
run time, called out directly in `hooks.rs`'s own module doc as
"real, substantial architectural complexity deliberately not tackled
yet."

## Why these two are architecturally different from the other four

`prestart`/`createRuntime`/`poststart`/`poststop` all run in the
*runtime's own process* (this project has no persistent daemon, so
that's simply whatever process called `run_reporting_pid`) — 0035's
own synchronization-pipe approach (the container's forked child blocks
on a pipe read; the parent runs the hooks, then writes one byte to
unblock it) works because the hook process and the container's own
namespaces are two entirely separate things.

`createContainer`/`startContainer` are different: the real spec
requires them to run **inside the container's own namespaces**
(checked directly against real runc's own
`libcontainer/rootfs_linux.go`/`standard_init_linux.go`, not the spec
prose alone — see below). A synchronization pipe to the parent
wouldn't help at all here; the hook process itself has to be spawned
from *inside* the forked child, at the exact right point in its own
setup sequence.

## The exact timing and status, verified against real runc source (not assumed)

```go
// libcontainer/rootfs_linux.go, finalizeRootfs, right before pivotRoot/msMoveRoot/chroot:
s.Status = specs.StateCreating
iConfig.Config.Hooks.Run(configs.CreateContainer, s)

// libcontainer/standard_init_linux.go, right after writing+closing the exec fifo,
// right before the final execve:
s.Status = specs.StateCreated
l.config.Config.Hooks.Run(configs.StartContainer, s)
```

This maps almost exactly onto this project's own existing
`ChildSetup::mount_pivot_and_exec` structure, which already has two
matching anchor points:

* `createContainer` runs right before executing the one
  `RootfsAction::PivotRoot` step in `self.plan` (found by `matches!`,
  not a new loop restructuring — `plan_rootfs_setup` always pushes
  exactly one `PivotRoot` action, unconditionally, so this is a single
  `if` inside the existing plan-execution loop). Status `"creating"`.
* `startContainer` runs right after the `exec_fifo` "start" wait
  unblocks (a no-op for plain `run`, which never sets `exec_fifo` —
  the timing relative to the final `exec` is identical either way),
  right before building the final `std::process::Command`. Status
  `"created"`.

Since this process already ran `unshare` (and, for a requested PID
namespace, already relay-forked into the grandchild that actually
becomes the namespace's pid 1) long before `mount_pivot_and_exec`
starts, it already *is* "the container's own namespaces" by the time
either hook point is reached — a hook spawned here via
`std::process::Command` (`crate::hooks::run`, entirely unchanged code)
automatically inherits every one of them, exactly like a real
`execve`'d hook process would. No new pipe, no new synchronization
primitive of any kind was needed — this is why `ContainerHooks`'s own
fields are plain, ordinary owned data (`id`/`oci_version`/
`bundle_path`/`annotations`/the two hook lists), cloned once into
`ChildSetup` at `build_child_setup` time, not a pipe descriptor.

## `createContainer` sees the host's own paths; `startContainer` doesn't

A real, easy-to-get-wrong subtlety, caught by manually verifying
against a real kernel before writing any automated test (this
project's own established discipline): `createContainer` runs
*before* `pivot_root`, so it still sees the exact same filesystem view
the runtime process itself does — an ordinary host-side temp path
works unchanged, exactly like `prestart`/`createRuntime` already rely
on. `startContainer` runs *after* `pivot_root` (which is itself part
of the same `self.plan` this hook point's own anchor sits right after)
— by the time it runs, `/` **is** the container's own rootfs. A
`startContainer` hook writing to `/tmp/some-host-path` would not see
that path at all; it has to write to a path inside its own root
instead (which the test suite then reads back from the host side at
`<bundle>/rootfs/...`, since that's the very same underlying
directory).

## Also correctly gained by the separate `create`/`start` two-phase lifecycle, not just `run`

`build_child_setup` (which now takes an `id: &str` parameter,
threaded through from `ocirun create`'s own CLI argument) is shared,
unmodified code between plain `run` and the separate `create`
(`launch::create`, which sets `exec_fifo`) — meaning
`mount_pivot_and_exec`'s two new hook-point anchors apply to both
without any extra code. Manually verified end to end: `ocirun create`
runs `createContainer` (status `"creating"`, a real pid); a later,
separate `ocirun start` unblocks the exec-fifo wait and runs
`startContainer` (status `"created"`) right before the container's own
command finally executes. `prestart`/`createRuntime`/`poststart`/
`poststop` still don't run for `create`/`start`/`kill`/`delete`
specifically (they're wired into `run_reporting_pid`, which `create`
never calls) — that remains a separate, not-yet-tackled gap.

## Failure semantics match `prestart`/`createRuntime`, not `poststart`/`poststop`

A failing `createContainer` or `startContainer` hook is **fatal**
(`fail`, terminates the process with `SETUP_FAILURE_EXIT_CODE`) —
these two hook points exist specifically so a hook can reject the
container before it actually runs the user's command, matching real
runc's own behavior (a failing `Hooks.Run` call there returns an error
that aborts the whole init sequence) and this project's own existing
`prestart`/`createRuntime` semantics. Manually and automatically
verified: a failing `createContainer` hook both fails the whole
`ocirun run`/`create` and provably prevents `startContainer` from ever
running at all.

## A small, unrelated rename to avoid a name collision

`launch.rs` already had a private `enum Hook { Poststart, Poststop }`
(an internal selector for `run_lifecycle_hooks`, nothing to do with
the real spec's `Hook` type). Importing
`oci_spec_types::runtime::Hook` for `ContainerHooks`'s own two `Vec<
Hook>` fields collided with it outright (E0255: "the name `Hook` is
defined multiple times"). Renamed the private enum to `HookPoint` —
a small, purely mechanical, unrelated-to-the-real-feature rename,
confirmed by grepping for every one of its (few) use sites.

## Real, manual verification against a real kernel first

Before writing any automated test: a real rootless bundle with both
hooks configured, confirming `createContainer` receives `status:
"creating"`, a real positive pid, and can still write to a host-only
temp path; `startContainer` receives `status: "created"` and can only
write inside its own (by-then-pivoted) root; a failing `createContainer`
hook aborting the whole container and provably preventing
`startContainer` from ever running; and the entire `create`/`start`
two-phase lifecycle exercising the exact same two hook points
correctly. A bundle with no hooks at all still runs exactly as before.

## Real, automated tests

Five new cases added to `tests/tests/ocirun_hooks.rs`, extending the
same pattern 0026/0035's own tests already established: `createContainer`
receiving the right state (host paths still visible);
`startContainer` receiving the right state (container-rootfs paths
only); a failing `createContainer` aborting the container and
preventing `startContainer` from running; `createContainer` provably
finishing before `startContainer` starts (a shared order log, written
from both sides of the pivot); and the pre-existing "no hooks
configured" test already covers this.

## Performance — hot-path change (`oci-runtime-core::launch`, `ocirun`'s own `create`/`run` call path), A/B re-verified

Touches `oci-runtime-core::launch` directly and changes
`launch::create`'s own signature (called from `ocirun`'s `cmd_create`)
— both explicitly named in this project's own "always re-verify"
list. A `git stash`/`git stash pop` A/B `hyperfine` comparison against
a hookless bundle (every bundle this project's own benchmark has ever
used) showed **1.09× "faster" before**, within the same noise band
this project has repeatedly measured for other zero-cost-when-unused
changes (0.4-0.5ms σ on a ~3ms mean). No plausible regression
mechanism: the added cost for a hookless bundle is exactly one
`matches!` check per already-iterated plan action (an enum-variant
comparison, no allocation) plus one `Option` check after the
exec-fifo wait — both effectively free, and `container_hooks` itself
is `None`, so no clone of any hook list ever happens either.

## What's still not here

* `prestart`/`createRuntime`/`poststart`/`poststop` for the
  `create`/`start`/`kill`/`delete` two-phase lifecycle specifically
  (they're wired into `run_reporting_pid` only) — a separate,
  not-yet-tackled gap, unrelated to this increment's own scope.
* Automated failed-systemd-scope cleanup, `-v`/`--volume`'s own more
  advanced options, the build cache, `ONBUILD`/`HEALTHCHECK` — all
  still exactly as earlier increments left them.
