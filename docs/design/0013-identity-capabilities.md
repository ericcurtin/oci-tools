# Design note 0013: `identity` — capabilities, uid/gid, `no_new_privileges`

Status: implemented (eleventh increment of milestone 3)
Scope: `oci_runtime_core::identity`, wired into `launch::ChildSetup::
mount_pivot_and_exec` right after the rootfs setup sequence and before
`exec`.

## The gap this closes

0012 assembled a working `ocirun run`, but flagged an honest omission:
the spec's declared `process.user` (uid/gid/supplementary groups) and
`process.capabilities`/`no_new_privileges` were parsed (0003) but never
actually *applied* — every container ran as whatever identity the
forked child happened to have after the rootless ID-mapping dance
(container-root, full default capability set inherited from `ocirun`
itself, `no_new_privileges` never set). A spec asking for a non-root
container user, a reduced capability set, or `no_new_privileges` was
silently ignored. This increment closes that gap.

## Ported from real `crun`, not re-derived from man pages

The application order is unforgiving — get it wrong and either the
wrong capabilities end up effective, or a later step fails because an
earlier one already dropped the privilege it needed. Rather than derive
this from `capabilities(7)` alone, I read `crun`'s own
`set_required_caps`/`libcrun_container_setgroups`
(`~/git/crun/src/libcrun/linux.c`) and ported its exact sequence:

1. `setgroups(2)` for `additionalGids` — but *skipped* entirely when
   `/proc/self/setgroups` already reads `deny`. The rootless ID-mapping
   dance (0006's `write_id_mappings`) writes exactly that whenever a GID
   mapping is present, and the kernel refuses `setgroups(2)` outright
   once it does — matching crun's own `can_setgroups` check verbatim,
   not a guess.
2. Drop every bounding-set capability not requested
   (`prctl(PR_CAPBSET_DROP)`, one capability at a time — the kernel has
   no bulk form) while still privileged enough to (`CAP_SETPCAP`).
3. `prctl(PR_SET_KEEPCAPS, 1)` so a `0 -> non-zero` UID transition next
   doesn't wipe the sets the `capset(2)` call right after sets
   explicitly anyway.
4. `setresgid(2)` then `setresuid(2)` to `process.user`.
5. `capset(2)` for effective/permitted/inheritable.
6. Clear the ambient set, then raise exactly the requested ambient
   capabilities (again, one `prctl(PR_CAP_AMBIENT_RAISE)` call each).
7. `prctl(PR_SET_NO_NEW_PRIVS, 1)` if the spec asks for it — last,
   matching crun (seccomp would go here too, but that's still a gap;
   see below).

`rustix::thread` already wraps every one of these as a safe function
(`capabilities`/`set_capabilities`, `remove_capability_from_bounding_
set`, `configure_capability_in_ambient_set`, `clear_ambient_capability_
set`, `set_keep_capabilities`, `set_no_new_privs`) — no raw `libc::`
calls needed for the capability/prctl half at all. `setresuid(2)`/
`setresgid(2)`/`setgroups(2)` themselves are **not** in rustix (same
reasoning as `fork()`: changing a *thread's* credentials without
glibc's whole-process-broadcast wrapper is exactly the kind of sharp
edge rustix declines to paper over), so those three use `libc`
directly, matching the crate's existing precedent.

## Verified against a real kernel, not just unit-tested parsing

Two layers, same pattern every increment since 0004 has used:

* **Unit tests** (in `identity.rs`) cover the capability-name parsing
  table and the `setgroups`-denied file check — pure logic, no
  syscalls.
* **Real, automated, end-to-end tests** (`tests/tests/ocirun_run.rs`,
  two new cases) run the actual built `ocirun` binary against a real
  busybox rootfs and `grep` the container's own `/proc/self/status`:
  * the spec's default capability set (`CAP_AUDIT_WRITE | CAP_KILL |
    CAP_NET_BIND_SERVICE`) shows up as the exact bitmask
    `0000000020000420` in `CapPrm`/`CapEff`/`CapBnd`, empty
    `CapInh`/`CapAmb`, and `NoNewPrivs: 1` — proving the whole chain
    (name parsing -> bounding drop -> capset -> no_new_privs) actually
    took effect inside the running container, not just that the code
    compiled.
  * an explicit empty capability set + `no_new_privileges: false`
    zeroes `CapEff`/`CapBnd` and clears `NoNewPrivs` — proving the
    "grant nothing" path works too, not only the default.

I also manually ran this against a wider set of cases before writing
the automated tests (a real scratch bundle, `busybox id` +
`/proc/self/status`, deleted after): confirmed `uid=0 gid=0` matches
`process.user`, and — a genuine, not-a-bug finding — the container's
`groups` list showed the *overflow* GID (`65534`) repeated once per
supplementary group the host user actually belongs to. That's correct:
this rootless setup maps only container GID 0, so every other
supplementary GID the calling host user has is unmapped inside the new
GID namespace and the kernel presents it as the overflow ID — the same
thing real `crun` shows for the identical case (no subordinate GID
range configured).

## What's still not here

* POSIX rlimits (`process.rlimits`) — a straightforward `setrlimit(2)`
  loop with no ordering subtlety, deliberately left for a follow-up
  increment to keep this one's real risk (capability/uid/gid ordering)
  isolated and reviewable on its own.
* seccomp — unimplemented; crun applies it in the `no_new_privileges`
  branch, so it interacts with this increment's ordering when it lands.
* SELinux/AppArmor process labels — out of scope for this milestone.
