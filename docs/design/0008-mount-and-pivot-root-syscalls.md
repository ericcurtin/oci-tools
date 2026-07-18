# Design note 0008: mount(2)/pivot_root(2) syscall wrappers (milestone 3, part 6)

Status: implemented (sixth increment of milestone 3)
Scope: `oci_mount::syscalls`. The last low-level building block before
`create` can actually assemble a container's filesystem — still no CLI
wiring, no fork/clone, no cgroups.

Continues 0003–0007. 0007 built "what do these mount options mean";
this increment builds "make the kernel call that does it": one
`mount(2)` per OCI mount entry, correctly dispatched to whichever of the
three very different shapes that syscall can take, plus `pivot_root(2)`.

## `oci_mount::syscalls`

* `plan_mount(&ParsedMountOptions) -> MountPlan` — pure decision logic:
  `Plain` (ordinary `mount(source, target, fstype, flags, data)`),
  `Remount` (`"remount"` was among the options — `mount(NULL, target,
  NULL, MS_REMOUNT|flags, data)`, fstype dropped since the kernel ignores
  it), or `Move` (`MS_MOVE` — `mount(source, target, NULL, MS_MOVE,
  NULL)`; not currently reachable from `parse_mount_options` since no OCI
  option maps to it, but part of the flag space it computes over, so
  handled rather than silently mis-dispatched).
* `mount(source, target, file_system_type, &ParsedMountOptions)` —
  executes the plan via `rustix::mount::{mount, mount_remount,
  mount_move}`.
* `pivot_root(new_root, put_old)` — thin wrapper over
  `rustix::process::pivot_root`.

Same split as `oci_runtime_core::namespaces`' `clone_flags_for`/`unshare`:
the interesting decision (which of three shapes, what flags survive) is
pure and unit-tested (5 new tests); the syscall itself is a thin,
manually-verified wrapper.

**Explicitly not this module's job**: the *sequence* of calls a real
mount entry needs. The kernel does not accept most flags atomically
together with `MS_BIND` in one call — 0007's `strace` trace already
showed runc doing exactly two calls for a read-only bind mount (bind,
then `MS_REMOUNT|MS_RDONLY|MS_BIND`). Deciding *when* an OCI mount entry
needs that two-step dance requires looking at the whole entry, not just
one resolved flag set — that's `create`'s job, next, using this module's
`mount()` as the one-call primitive it's built from. Test
`rbind_readonly_plans_as_a_single_plain_bind_mount` names this
explicitly so it's never mistaken for an oversight.

## Manually verified against the real kernel, not just rustix's docs

A scratch Cargo project (path-dependent on the real `oci-mount` and
`oci-spec-types` crates, built with the workspace's pinned toolchain,
run, and deleted — not committed) did, as an ordinary unprivileged user:

1. `unshare(NEWUSER | NEWNS)`, then wrote `uid_map`/`gid_map`/`setgroups`
   (same dance as 0006), then made `/` recursively private so nothing
   that followed could propagate to the real host mount namespace.
2. Called the real `oci_mount::mount()` to mount a `tmpfs` with
   `nosuid,mode=777` — succeeded, and a file written into it read back
   correctly, and showed up in `/proc/self/mountinfo`.
3. Called `oci_mount::mount()` again with `remount,ro,bind` on the same
   target — succeeded, and a subsequent write failed as expected.
4. Bind-mounted a directory onto itself (the standard trick to give
   `pivot_root` a target that is its own mount point), then called the
   real `oci_mount::pivot_root()` — succeeded; `/`'s new entries were
   exactly the old root, relocated to `old_root`, confirming the pivot.
5. After the scratch process exited, confirmed on the **real host**:
   the original `/tmp/mount-scratch-target` directory was empty (the
   tmpfs and its `hello` file never existed outside the disposable mount
   namespace) and `mount` showed no leaked entries.

Every one of those five things is a real kernel operation this crate's
code performed and this session watched succeed or fail correctly,
cross-checked against host state before and after — not an inference
from documentation.

## Why still no automated syscall test

Same reason as 0006: `unshare(NEWUSER)` requires the calling process be
single-threaded, and `cargo test`'s harness never is, even filtered to
one test. `plan_mount`'s decision logic is fully unit tested; `mount()`/
`pivot_root()` themselves get real coverage as part of `create`'s own
subprocess-based tests (the built `ocirun` binary, spawned fresh, is a
single-threaded process from the moment it starts — the same shape as
the scratch program above).

## Decisions and risks

* `oci-mount` now depends on `rustix` (features `fs`, `mount`, on top of
  the `thread`/`system`/`process` the workspace already enables for
  `oci-runtime-core`) — no new capability-group entry needed; still the
  same one crate this workspace picked for "low-level unix syscalls".
* Still no cgroup, loop-device, or overlayfs code — next.
