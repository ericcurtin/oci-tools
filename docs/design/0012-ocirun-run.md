# Design note 0012: `ocirun run` (milestone 3, part 10)

Status: implemented (tenth increment of milestone 3)
Scope: `oci_runtime_core::launch`; `ocirun run`. This is the assembly
increment 0011 flagged as coming next: every primitive built across
0003–0011 wired together into an actual, working container.

## `oci_runtime_core::launch`

`run(&Bundle, &rootfs) -> io::Result<i32>`: forks (0011), `unshare`s
(0006), writes rootless ID mapping if needed (0006), runs the planned
rootfs setup (0008/0010) via the real `mount(2)`/`pivot_root(2)` calls
(0008), and `exec`s the container's process — returning the same exit
code the container's own process would report to its own shell.

**The one new architectural piece this increment needed**: when a new
PID namespace is requested, the process that calls `unshare(CLONE_
NEWPID)` does **not** join it — only that process's *next forked child*
does, becoming the new namespace's pid 1. Mounting a fresh `/proc` (which
every default bundle's mount list does) requires actually owning the pid
namespace it reflects. Caught immediately by running the assembled
pipeline for real: without a second, inner fork, `mount("proc", ...)`
failed `EPERM` every time. Fixed by having the first forked child
`unshare` and map IDs, then fork *again* when `NEWPID` was requested; the
grandchild becomes pid 1 and does the actual mount/`pivot_root`/`exec`
sequence, while the first child relays the grandchild's exit status as
its own (so the outer `run` — waiting on the first child — sees the
right code either way).

Two more real, previously-undiscovered gaps surfaced by actually running
the assembled pipeline (not by re-reading source or re-inspecting a
plan's shape — every earlier increment's verification method, pushed one
level further now that there's a real container to run):

* **The cgroup-v1-vs-v2 mount-type gap 0010 already flagged** turned out
  to be exactly as scoped: `plan_rootfs_setup` now substitutes `cgroup2`
  for the literal `"cgroup"` mount type unconditionally (oci-tools has no
  v1 host to fall back to), confirmed against the same real `strace`
  evidence 0007/0010 already captured.
* **A second, new wrinkle in the rootless `/sys` path**: with `/sys`
  bind-mounted from the host (rootless containers can't mount a fresh
  `sysfs` — see `Spec::into_rootless`), `/sys/fs/cgroup` is *already*
  part of that same recursive bind, so a separate, explicit `cgroup2`
  mount there is redundant — the kernel returns `EBUSY` (something's
  already mounted) or `EPERM` depending on the exact permission path.
  Nothing more needs to happen: the container already sees a real
  cgroup2 hierarchy at that path. Tolerated (logged via `tracing::warn!`,
  not treated as a failure) alongside the already-documented rootless
  `/sys` read-only-remount `EPERM` from 0010.

Both tolerated failures are narrowly scoped to their exact, understood
conditions (specific action type, specific `io::ErrorKind`s) — not a
blanket "ignore mount errors" — so an unrelated real failure still fails
the container as it should.

## Real, automated, end-to-end tests — not manual scratch verification

0011 flagged this in advance and it held: `tests/tests/ocirun_run.rs`
spawns the *built* `ocirun` binary (the same pattern `ocirun_state.rs`/
`ocirun_spec.rs` already use), which starts fresh and single-threaded
from its own `main()` regardless of the test harness's own thread count
— exactly what `unshare(CLONE_NEWUSER)` requires. Four tests, all
exercising a real busybox-based rootless bundle end to end: the
container's own output and `pivot_root`ed view of `/`, the container's
exit code propagating as `ocirun`'s own exit code (including a nonzero
one), `exec` failure reporting exit `127`, and `sethostname` actually
taking effect inside the container. `busybox` isn't a hard CI
dependency: the tests check for it on `$PATH` and skip themselves
(printing why) rather than fail if it's missing, matching the "gated"
pattern `oci-mount`'s original design doc anticipated for privileged
tests, generalized here to "gated on tool availability".

## Measured against the real thing: beats both runc and crun

The actual point of this whole project. `hyperfine` (100+ samples per
tool, 5 warmup runs), same rootless busybox bundle, `/bin/true` as the
container's process, all three tools' own `run` subcommand (create +
start + wait, matching what this increment implements):

| tool | mean | min | max | relative |
|---|---:|---:|---:|---:|
| `ocirun` | 2.8 ms | 1.8 ms | 4.4 ms | 1.00× |
| `crun` | 10.1 ms | 6.6 ms | 14.5 ms | 3.59× slower |
| `runc` | 20.5 ms | 16.3 ms | 26.4 ms | 7.31× slower |

`ocirun run` is **3.6× faster than crun and 7.3× faster than runc** for
a full create-start-wait-destroy cycle of a trivial rootless container,
on this session's own aarch64 host (20 vCPUs, real hardware, not a
shared/throttled CI runner — numbers will differ elsewhere, but the
relative gap is the point). No profiling or tuning went into this;
it's the natural result of a `~2.4 MB` static-ish Rust binary with no
runtime/GC/interpreter startup cost, doing exactly the syscalls needed
and nothing else.

## A real distro-hardening gap this surfaced: Ubuntu's unprivileged-userns AppArmor restriction

Running `tests/tests/ocirun_run.rs` for real inside the `ubuntu-26.04` CI
VM (not just on this session's own dev host) failed every rootless test
with `writing id mappings: Permission denied`, even though the exact
same code path had already been manually verified against a real kernel
earlier in this milestone. Reproduced independently of any oci-tools
code at all — `unshare --user --map-root-user -- whoami` (the
`util-linux` tool, nothing to do with this project) fails identically
in that VM — confirming this is a genuine distro/kernel policy, not a
bug: Ubuntu 24.04+ auto-transitions any *unconfined* process that calls
`unshare(CLONE_NEWUSER)` into a restrictive built-in AppArmor profile
(`kernel.apparmor_restrict_unprivileged_userns=1` by default; confirmed
via `dmesg`, which showed the kernel's own audit line: `apparmor="DENIED"
... capability=21 capname="sys_admin"`). That confinement denies the
`CAP_SYS_ADMIN` check the kernel does before accepting a write to the
new namespace's own `/proc/<pid>/uid_map` — so rootless namespace
creation fails out of the box for **every** rootless container runtime
alike (crun, runc, bubblewrap, rootless podman/docker...), not something
specific to this implementation.

The standard, real-world fix (verified interactively in the CI VM,
including confirming it does *not* apply to non-matching binary paths)
is the same one Ubuntu's own containerized-app packages (docker.io,
podman) ship: an AppArmor profile granting the specific binary `userns,`
under an `unconfined` flag. `ci/vm-prepare.sh` now loads exactly such a
profile, scoped to this workspace's own binary names under any
`target/{debug,release}/` path, so CI exercises the identical rootless
path a real deployment does rather than papering over it with the
blunter, security-reducing `sysctl -w
kernel.apparmor_restrict_unprivileged_userns=0`. This is a **CI-only**
fix, scoped to build-tree paths — it is not yet a packaging story for
`ocirun`/`ociman` themselves; a real `.deb` install on Ubuntu 24.04+
will need the same kind of profile shipped and loaded against its own
fixed install path (e.g. `/usr/bin/ocirun`), which is out of scope until
this project has a packaging increment.

## What's still not here

* The separate `create`/`start`/`kill`/`delete`/`exec` two-phase
  lifecycle (`ocirun run` combines create+start, foreground only) —
  needs a persistent background process and state-store integration
  with a live pid, deliberately out of scope for this increment (see
  0011).
* Capabilities, `no_new_privileges`, seccomp — `Spec`'s
  `LinuxCapabilities`/`no_new_privileges` fields are parsed (0003) but
  not yet applied to the container process before `exec`.
* cgroup *directory* creation and the container process's own migration
  into it — 0009 built the resource-limit *translation*, not this.
