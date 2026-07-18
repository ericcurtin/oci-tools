# Design note 0010: rootfs setup sequencing (milestone 3, part 8)

Status: implemented (eighth increment of milestone 3)
Scope: `oci_runtime_core::rootfs`. Still no CLI wiring, no fork/clone —
this closes the last gap `oci_mount::syscalls` (0008) explicitly left
open ("building the sequence... is `create`'s job"), so `create` (next)
has every piece it needs to actually assemble a container.

## `oci_runtime_core::rootfs`

`plan_rootfs_setup(&Bundle, &rootfs) -> Vec<RootfsAction>` — pure logic,
touches the filesystem *not at all*, deciding the ordered sequence a
container's filesystem setup needs: make the mount tree private, bind the
rootfs onto itself (a `pivot_root` prerequisite), one `Mount` per bundle
mount entry (splitting a combined bind+readonly request into the real
two-call bind-then-remount dance the kernel requires — confirmed via the
0007/0008 `strace` trace), a bind+remount for each `readonlyPath`, a
`MaskPath` for each `maskedPath`, `pivot_root` + unmount-old-root +
`chdir("/")`, a bind+remount if `root.readonly`, and `sethostname` if the
bundle sets one and has a UTS namespace.

## Two real bugs caught by actually executing a generated plan

This increment's verification methodology (0006/0008's manual-scratch-
program approach, extended) is what caught these — neither would have
been obvious from reading runc's source or a static `strace` trace alone:

**1. Masked-path file-vs-directory classification cannot happen during
planning.** An earlier version of this module `stat`-ed each masked path
*while planning*, before any of the plan's own mounts had run, to decide
`tmpfs`-over-a-directory vs. bind-`/dev/null`-over-a-file. That's wrong
for masked paths a *later* action in the very same plan brings into
existence: `/proc/kcore`, `/proc/keys`, and other standard `maskedPaths`
entries are procfs-provided pseudo-*files* that don't exist at all until
`/proc` is mounted. A pre-mount `stat` sees "missing", plans a
directory-shaped `tmpfs` mount, and that mount then fails with `ENOTDIR`
once `/proc` has actually turned the path into a file. Running the
generated plan against a real kernel (see below) hit this immediately.
**Fixed** by introducing `RootfsAction::MaskPath` (carrying only the
target) and [`classify_masked_path`], to be called by whoever *executes*
the plan, immediately before acting on that step — by construction, every
earlier action (including the mount that might have created the path)
has already run by then.

**2. A masked path that doesn't exist must be skipped, not masked.**
`classify_masked_path` returns `Missing` for a path that isn't there —
checked against runc's actual `maskPaths` (`libcontainer/rootfs_linux.go`:
"open the target path; skip if it doesn't exist"). The first fix attempt
treated `Missing` the same as `Directory` (mount a fresh `tmpfs`), which
fails outright for masked paths under `/proc` that never exist on a given
kernel build (`/proc/timer_stats`, `/proc/sched_debug` on this session's
kernel) — `/proc` is a virtual filesystem; there's no directory entry to
create a mount point at. There's also nothing to protect if the kernel
never exposed the path in the first place, so skipping is correct, not
just expedient.

## Verified by executing a generated plan against a real kernel

Not just checking the plan's *shape* (0009's fixture-based unit tests do
that): a scratch program (path-dependent on the real crates, built with
the pinned toolchain, run, and deleted — not committed) built a minimal
busybox bundle, generated `oci_runtime_core::plan_rootfs_setup`'s real
output for it, and **executed every action** via `oci_mount::syscalls` —
`unshare`d into a fresh user+mount+uts+pid namespace (computing the flags
with the real `oci_runtime_core::namespaces::clone_flags_for`, from the
bundle's own `linux.namespaces`), forked so the child could legitimately
own a new PID namespace (required for a fresh `proc` mount to succeed —
`unshare(CLONE_NEWPID)` only affects the *next* forked child, never the
calling process itself), then ran the whole plan and finally `exec`'d the
container's process. Confirmed, against real kernel state:

* `/proc`, `/dev`, `/dev/pts`, `/dev/shm`, `/dev/mqueue` all mounted and
  usable (`/proc/self/mounts` had 62 entries; a file written to
  `/dev/shm` read back correctly).
* `sethostname` took effect inside the container (`hostname` printed the
  bundle's configured name) without affecting the host's.
* Masked directories (`/proc/acpi`) were empty and readable; masked files
  (`/etc/secret`, planted with real secret content in the test rootfs)
  read back empty via the `/dev/null` bind, while the **host's copy of
  the same file was completely untouched** afterward.
* `pivot_root` actually relocated the root: a directory listing of `/`
  inside the container showed only the bundle's own rootfs contents.
* With `root.readonly` (the default), a write to `/` failed with "Read-
  only file system" — proving the final bind+remount-readonly on the new
  root took effect.
* After the scratch process exited: no leaked mounts on the host, and the
  host's real files were unaffected.

## Two more gaps found and *not* fixed this increment (documented, not silently worked around)

* **Rootless `/sys` cannot always be remounted read-only.** Real rootless
  containers bind-mount the host's `/sys` instead of mounting a fresh
  `sysfs` (a fresh `sysfs` mount requires owning a real network
  namespace, which rootless mode deliberately drops — see
  `Spec::into_rootless`'s own comment). The follow-up read-only remount
  on that bind needs `CAP_SYS_ADMIN` in the namespace that owns the
  *original* `/sys` superblock (the host's), which a fake-root-in-a-
  userns does not have; it failed with `EPERM` in this session's
  verification and had to be tolerated to continue testing the rest of
  the plan. This is a known rootless-container rough edge across the
  ecosystem, not specific to this implementation; `create` will need to
  decide whether to tolerate the failure (matching some tools'
  documented behavior) or find another way to constrain `/sys`.
* **No cgroup-v1-vs-v2 auto-detection for the `cgroup` mount type.** The
  bundle's `sys/fs/cgroup` mount entry declares filesystem type
  `"cgroup"` (the OCI spec's traditional, pre-v2 vocabulary); real runc
  auto-detects a cgroup-v2-unified host and substitutes fstype
  `"cgroup2"` before actually calling `mount(2)` (confirmed in 0007's
  `strace` trace: `mount("cgroup", ..., "cgroup2", ...)`).
  `plan_rootfs_setup` passes the literal spec value through unchanged, so
  this mount failed in the verification run exactly as it would with no
  translation at all. oci-tools targets cgroup v2 exclusively, so this
  translation is unconditional (no v1 fallback to consider) — a small,
  well-scoped fix, deliberately left for a future increment rather than
  folded into this one.

## Decisions and risks

* Still no fork/clone, no CLI wiring; `create` (next) is the first real
  caller of everything built across 0003–0010.
* The shared-tmpfs optimization real runc uses for masked directories
  (mount one read-only tmpfs once, bind-mount it to every other masked
  directory instead of mounting a fresh tmpfs per directory) is not
  implemented — each masked directory gets its own `tmpfs` mount. A
  correctness-neutral performance detail, not a behavior gap; worth
  revisiting once there's a benchmark to justify it against.
