# Design note 0014: `rlimits` (POSIX resource limits)

Status: implemented (twelfth increment of milestone 3)
Scope: `oci_runtime_core::rlimits`, wired into `launch::ChildSetup::run`
as the very first step (before `unshare`).

## The gap this closes

0013 closed the uid/gid/capabilities/`no_new_privileges` gap and
explicitly flagged `process.rlimits` as the deliberately-deferred
follow-up, to keep that increment's real risk (capability/uid/gid
ordering) isolated and reviewable on its own. This increment closes it:
`setrlimit(2)` for every entry in `process.rlimits`.

## Simpler than `identity` — and where it's applied matters more than how

Unlike capabilities, there's no multi-step ordering dance: each
`setrlimit(2)` call is independent of every other one, and of the
uid/gid/capability drop. `rustix::process` already wraps it safely
(`setrlimit`/`getrlimit`/`Resource`/`Rlimit`) — no raw `libc::` call
needed here at all, unlike `identity`'s `setresuid`/`setresgid`/
`setgroups`.

What *does* matter is *when* it runs. Real `crun`'s own
`libcrun_set_rlimits` call is commented, verbatim, "This must be done
before we enter a user namespace" (`~/git/crun/src/libcrun/
container.c`) — rlimits are inherited across `fork(2)`, so crun applies
them to the *parent* process before ever creating the container's
namespaces, so *raising* one above its current hard limit (which needs
`CAP_SYS_RESOURCE`) is only ever attempted with whatever privilege the
invoking process already has, not whatever a fake-root-in-a-userns
happens to be granted. This project's simpler single-fork model doesn't
keep a separate long-lived parent the way crun does, but the same
principle still applies to the forked child: `rlimits::apply` runs as
the very first thing in `ChildSetup::run`, strictly before `namespaces::
unshare`.

## A real, not-a-bug gotcha this surfaced: `RLIMIT_NPROC` is host-uid-wide

Manually verifying this against a real kernel (a scratch busybox
bundle, `config.json` requesting both `RLIMIT_NOFILE` and
`RLIMIT_NPROC`, deleted after) hit `forking container pid 1: Resource
temporarily unavailable` (`EAGAIN`) — not a bug in this code. `RLIMIT_
NPROC` is enforced against the process's *real* uid's total process
count across the whole system, not per-namespace: on a real desktop/CI
host where that uid already has more running processes than a small
requested `RLIMIT_NPROC` soft limit allows, the very next `fork(2)`
(the PID-namespace relay fork `docs/design/0012` added) fails
immediately. Real `runc`/`crun` have the exact same behavior — it's a
documented, long-standing rough edge of `RLIMIT_NPROC` on Linux, not
something a container runtime can paper over. `tests/tests/
ocirun_run.rs`'s new automated test deliberately exercises only
`RLIMIT_NOFILE` for this reason (a low `RLIMIT_NPROC` would make the
test's pass/fail depend on how many other processes the CI/dev
machine's user happens to have running at the time).

## Verified against a real kernel

* Unit tests (`rlimits.rs`): the name -> `Resource` lookup table (all
  16 names `crun`'s own `rlimits[]` table supports) and the
  `RLIM_INFINITY` -> `None` conversion — pure logic, no syscalls. A
  real `setrlimit(2)` round-trip is deliberately *not* a unit test:
  `cargo test` runs every test in one shared process by default, and a
  lowered `RLIMIT_NOFILE` would leak across concurrently running
  sibling tests.
* Real, automated, end-to-end test (`tests/tests/ocirun_run.rs`,
  `run_applies_rlimits`): runs the actual built `ocirun` binary against
  a real busybox rootfs requesting `RLIMIT_NOFILE` soft=256/hard=512,
  and greps the container's own `/proc/self/limits` to confirm both
  values took effect for real.
* Manually verified beyond what's automated (scratch bundle, deleted
  after): the `RLIMIT_NPROC`/`EAGAIN` interaction above, and that an
  omitted `rlimits` list (the common case) is a complete no-op, not an
  error.

## What's still not here

* seccomp — the last piece of `oci_runtime_core::identity`'s
  `no_new_privileges` interaction (crun applies seccomp inside that
  same branch); still entirely unimplemented.
* cgroup *directory* creation and the container process's own
  migration into it (0009 built only the resource-limit translation).
* The separate `create`/`start`/`kill`/`delete`/`exec` two-phase
  lifecycle — `ocirun run` still combines create+start, foreground
  only.
