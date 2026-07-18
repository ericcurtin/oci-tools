# Design note 0015: cgroup directory creation + process migration

Status: implemented (cgroupfs driver only; thirteenth increment of
milestone 3)
Scope: `oci_spec_types::runtime::Linux::cgroups_path`,
`oci_runtime_core::cgroups::{directory_for, enter}`, wired into
`launch::ChildSetup::run` strictly before `unshare`.

## The gap this closes

0009 built `plan_resources`/`apply` — translating `LinuxResources` into
cgroup v2 interface-file writes — but explicitly scoped that to *writes
into an already-existing directory*; nothing yet decided *which*
directory, created it, or moved the container's own process into it.
This increment closes that: `linux.cgroupsPath` (a new, previously
entirely-unparsed field) is resolved to a real directory under
`/sys/fs/cgroup`, created, has its resource limits written, and the
container process migrates itself in — all before the container's own
`CLONE_NEWCGROUP` `unshare(2)`.

## Scope: `cgroupfs` driver only, no synthesis, no systemd delegation discovery

Matching the module doc's own long-standing roadmap ("cgroups v2 with
both systemd and cgroupfs drivers" — plural, two separate pieces of
work), this increment is the `cgroupfs` driver alone:

* `cgroupsPath` is interpreted the way `runc`'s own `fs2` driver does
  when *not* using the systemd driver: a plain path relative to the
  cgroup v2 mount root, not the systemd-driver's `slice:prefix:name`
  form (rejected as unrecognized for now, not silently misinterpreted).
* Unlike `runc`, an unset `cgroupsPath` does **not** get a synthesized
  default. Real `runc` falls back to a `--cgroup-parent`-derived name
  when unset; this project has no equivalent CLI convention yet, and
  guessing one that happens to match nothing real would be worse than
  the honest "no cgroup management for this container" that leaving it
  `None` produces.
* Discovering a rootless user's *delegated* subtree automatically
  (querying `systemd`'s user manager over D-Bus, the way real rootless
  `podman`/`crun` do) is not attempted. The caller (eventually
  `ociman`, analogous to `podman`/`conmon`) is expected to already know
  and supply a valid, writable `cgroupsPath` — exactly the same
  contract `runc`/`crun` themselves have with *their* callers in
  `cgroupfs`-driver mode.

## Two real cgroup v2 kernel constraints this surfaced — verified with plain shell commands, independent of this project's own code

Manually verifying this against a real kernel (this session's own dev
host, `systemd --user` instance) hit two genuine, well-documented
cgroup v2 rules before finding a setup that actually worked — both
reproduced with plain shell one-liners, no `oci-tools` code involved,
confirming neither is a bug here:

1. **Cross-branch migration needs write access to the common
   ancestor.** Writing a pid into `<some other branch>/cgroup.procs`
   from a process whose *own* current cgroup is a sibling branch (e.g.
   a plain SSH/login session's `session-N.scope`, never delegated)
   fails `EPERM` even for a bare `echo $$ > .../cgroup.procs` — the
   kernel requires write permission on the nearest common ancestor of
   the source and destination cgroups, and a login session's ancestor
   chain up to any *other* branch is root-owned. This is exactly why
   rootless container tools invoke `systemd-run --user --scope` (or
   equivalent) rather than just `write()`ing wherever they like: it
   asks systemd itself — which already owns the whole delegated
   subtree — to place the calling process correctly first.
2. **"No internal process constraint."** A cgroup that directly
   contains a process cannot have controllers enabled in its own
   `cgroup.subtree_control` for children — `echo +pids >
   .../cgroup.subtree_control` from *inside* a freshly created
   `systemd-run --user --scope --property=Delegate=yes` scope (i.e. a
   cgroup whose `cgroup.procs` already contains the shell running the
   command) fails `EBUSY`. Real systemd services with `Delegate=yes`
   solve this by keeping their main process one level up (e.g. under a
   slice) rather than the delegated leaf directly, or by moving into a
   nested cgroup themselves before enabling anything further down —
   this project's own manual verification worked once the target
   cgroup was made a *sibling* of the invoking process's own cgroup
   (both direct children of the already-delegated `app.slice`, whose
   `subtree_control` was already populated by systemd itself), not a
   child of it.

Neither of these is something a container runtime can paper over —
`runc`/`crun` are bound by the identical kernel rules.

## Ordering: enter the cgroup *before* `CLONE_NEWCGROUP`

`unshare(2)`'s cgroup namespace behavior mirrors the PID namespace
wrinkle 0012 found: the kernel roots a *new* cgroup namespace at
whatever cgroup the calling process is in **at unshare time**. Moving
the process into its target cgroup only *after* unsharing
`CLONE_NEWCGROUP` would root the new namespace at the host's cgroup,
not the container's — so `cgroups::enter` runs strictly before the
single combined `unshare(2)` call that includes `NEWCGROUP` (right
after `rlimits::apply`, for the same "before we give up any privilege
we might need" reasoning 0014 already established).

## Verified against a real kernel

* Unit tests (`cgroups.rs`): `directory_for`'s path-joining and `..`
  rejection, `enter`'s `cgroup.procs` write content — plain file I/O
  against a temp directory, no real cgroupfs needed (same style 0009
  already established for this module).
* Manually verified end-to-end (scratch bundle + `systemd-run --user
  --scope --slice=app.slice`, deleted after): the real `ocirun run`
  binary, invoked from inside a properly delegated scope, successfully
  created a sibling cgroup, wrote `pids.max`, migrated itself in, and
  the container's own `cat /proc/self/cgroup` printed `0::/` — proving
  the whole chain (directory creation, resource writes, migration,
  namespace-rooting order) end to end.
* **Real, automated, end-to-end test**
  (`tests/tests/ocirun_run.rs::run_creates_and_enters_the_requested_
  cgroup`): the same scenario, kept in the test suite rather than only
  manually verified once, but gated on a *real, functional* probe
  (`systemd-run --user --scope -- true` actually succeeding, not just
  checking the binary is on `$PATH`) — printing why and skipping,
  rather than failing, on a system with no reachable `systemd --user`
  D-Bus session (plausible on a minimal image with no login session or
  lingering enabled), matching the `busybox`-gating precedent 0012
  already established for a different real-environment dependency.

## What's still not here

* The systemd cgroup driver (`slice:prefix:name` parsing, D-Bus-based
  transient-unit creation, automatic delegated-subtree discovery).
* Cleaning up the cgroup directory on container exit — currently relies
  entirely on the kernel's own "remove an empty, unreferenced cgroup"
  behavior (verified informally: the directory this increment's manual
  test created was already gone by the time it went looking to clean
  up). No explicit `rmdir` on container exit yet, which matters once a
  proper create/start/delete lifecycle exists (0012 already flagged
  that lifecycle itself as unimplemented).
* Devices cgroup / eBPF device filtering — `LinuxResources.devices` is
  parsed (0003) but has no cgroup-v2-equivalent translation here (v2's
  device control is BPF-based, not a simple interface file like
  memory/cpu/pids).
