# Design note 0022: `ocirun exec`

Status: implemented
Scope: `oci_runtime_core::{nsenter, exec}`, `ocirun exec`.

## The gap this closes

The last piece of `ocirun`'s own lifecycle milestone 3 names
explicitly: running an *additional* process inside an already-running
container, as opposed to `create`/`run` (which only ever create a
container's *first* process, in brand-new namespaces). Builds directly
on the `create`/`start`/`kill`/`delete` lifecycle (0017): `exec` only
makes sense against a container that's actually running in the
background, tracked in `StateStore` with a live pid — exactly what
`create` already leaves behind.

## Ported from real `runc`'s own join order — verified against its source, not guessed

`setns(2)`-ing into a rootless container's namespaces in the wrong
order fails outright. Read `runc`'s own implementation
(`libcontainer/nsenter/nsexec.c`'s `join_namespaces`) rather than
guessing: its own comment explains precisely why order matters —
*"We first try to join all non-userns namespaces... We then join the
user namespace, and then try to join any remaining namespaces (this
last step is needed for rootless containers — we don't get `setns(2)`
permissions until we join the userns and get `CAP_SYS_ADMIN`)."* `runc`
does this as a 3-phase dance (attempt non-user namespaces, join user,
retry the failures) because it also supports joining an *externally
created* namespace unrelated to the container's own userns — this
project has no such feature, so `oci_runtime_core::nsenter` uses the
simpler 2-phase version that's actually sufficient here: join the user
namespace first, then everything else.

Also ported: opening every namespace's file descriptor *before* joining
any of them. Once the calling process has joined the container's mount
or user namespace, the host's own `/proc/<pid>/ns/*` paths may no
longer resolve the same way (a mount namespace change can hide the
host's `/proc`; a user namespace change can lose permission to read
another process's `/proc/<pid>/ns/*` at all) — `nsenter::open_all`
opens everything up front in the original namespace, matching `runc`'s
own `__open_namespaces`/`join_namespaces` split.

## Reusing the PID-namespace relay-fork trick from `create`/`run`

`setns(2)` into a PID namespace has the exact same wrinkle 0012 already
documented for `unshare(CLONE_NEWPID)`: it does not move the *calling*
process into the target namespace at all, only a *subsequent* forked
child becomes a member (and gets a namespace-relative pid). `oci_
runtime_core::exec::ExecSetup::run` forks again after joining, exactly
like `launch::ChildSetup::run` already does for `create`/`run` — the
same primitive (`process::fork`/`wait`, split out of `fork_and_wait`
back in 0017 for precisely this kind of reuse) serves both.

## No rootfs setup needed — that's the whole simplification versus `create`/`run`

Unlike `create`/`run`, `exec` does no mount/`pivot_root` work at all:
joining the container's existing mount namespace already puts the
calling (forked, namespace-joined) process inside the exact rootfs
view `pivot_root` established when the container was created. What's
left is exactly `identity::apply` (uid/gid/capability drop,
`no_new_privileges` — reused as-is from `launch`) followed by `exec`.

## Defaults: the container's own identity, not (yet) an override

`ocirun exec <id> <command>...` reads the target container's own
bundle back (`state.bundle`, already recorded) and reuses its
`process.user`/`capabilities`/`no_new_privileges`/`cwd`/`env` and
namespace list verbatim — matching real `runc exec`'s own default
behavior when no `--user`/`--cwd`/`--env` override is given, which
this increment doesn't implement yet (a real, narrow, honestly-flagged
gap: everyone who execs into a container gets the same identity the
container's own init process runs as).

## Verified against a real, running container

Manually verified end to end (a real `create`+`start`'d busybox
container running `sleep 30` in the background, deleted after):

* `hostname` inside the exec'd process printed the container's own
  hostname — proves the UTS namespace was actually joined.
* `ps aux` showed **both** the container's own init (`sleep 30`, pid 1)
  **and** the exec'd process itself, at a *different*,
  namespace-relative pid — proves the PID namespace join and relay
  fork both worked, and that pid numbering is genuinely
  container-scoped, not just "the exec'd process happens to run".
* `id` reported the exact same uid/gid/capabilities the container's own
  process runs as.
* A real `exit 5` from the exec'd command came back as `ocirun exec`'s
  own exit code, and the *container's own* process (checked via `ocirun
  state`, and `ps -p <pid>` from the host) was completely unaffected —
  still running, same pid, same command — proving `exec` genuinely adds
  a process alongside the existing one rather than disturbing it.

## Real, automated, end-to-end tests, then shared test helpers

Three new tests in `tests/tests/ocirun_exec.rs` reproduce all of the
above against the actual built `ocirun` binary (not manual-only, unlike
some earlier syscall-level increments where automating was infeasible —
`exec` spawns real subprocesses with no threading restriction, so a
full automated test is possible here). `ocirun`/`ocirun_create`/
`state_status`/`wait_for_status` (previously private to `ocirun_
lifecycle.rs`) moved to the shared `oci-tools-tests` crate so both
files share one implementation, continuing the pattern 0020/0021
already established for `ociman`'s own test helpers.

## What's still not here

* `--user`/`--cwd`/`--env` overrides for `exec` (see "Defaults" above).
* Lifecycle hooks (`prestart`/`createRuntime`/`startContainer`/...).
* The systemd cgroup driver and full multi-action seccomp profiles
  (already-flagged gaps, unaffected by this increment).
