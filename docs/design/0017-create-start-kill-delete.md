# Design note 0017: the separate `create`/`start`/`kill`/`delete` lifecycle

Status: implemented (fifteenth increment of milestone 3)
Scope: `oci_runtime_core::{exec_fifo, signal}`, `process::{fork, wait,
kill, alive}` (split out of/added alongside the existing `fork_and_wait`),
`launch::create`, `ocirun create`/`start`/`kill`/`delete`.

## The gap this closes

0012 shipped `ocirun run` (create+start combined, foreground) and
explicitly flagged the separate two-phase lifecycle as needing "a
persistent background process surviving after the CLI invocation
returns, and state-store integration with a live pid" — this increment
builds exactly that, the last major piece of `ocirun`'s own CLI surface
before milestone 3's remaining gaps become genuinely separate features
(the systemd cgroup driver, full multi-action seccomp, `exec`/hooks).

## Ported from real `runc`'s own exec-fifo mechanism

`create` leaves the container's init process blocked, waiting for
`start`, without any daemon or extra IPC channel beyond a single POSIX
FIFO — the same design real `runc` uses
(`libcontainer/container_linux.go`'s `createExecFifo`/`handleFifo`,
`standard_init_linux.go`'s wait), read from source rather than
reinvented: opening a FIFO for **write** blocks (absent `O_NONBLOCK`)
until a reader shows up, and vice versa. `create`'s container process
opens for write ([`exec_fifo::wait_for_start`]) and sits blocked doing
nothing else; `start` opens for read
([`exec_fifo::signal_start`]), which is what actually unblocks the
write-open; the container process then writes one byte (proving it's
alive and really about to `exec`) and `start` reads it back.

### A real bug this caught immediately: `pivot_root` breaks a by-path reopen

Running this against a real kernel for the first time failed
immediately with `ENOENT`: the container process only waits on the
fifo as the *last* step before `exec`, by which point `pivot_root` has
already swapped its view of `/` to the container's own rootfs — an
ordinary by-path `open(2)` at that point resolves against the
*container's* root, not wherever the fifo actually lives on the host.
Real `runc` solves exactly this (`init_linux.go`'s
`_LIBCONTAINER_FIFOFD`, `standard_init_linux.go`'s `pathrs.Reopen`) by
opening the fifo with `O_PATH` *before* `pivot_root` (a lightweight
reference that doesn't block on the FIFO and isn't affected by a later
mount-namespace change) and reopening it via `/proc/self/fd/<n>`
afterward — `/proc/self/fd` is a magic symlink resolved against the
*kernel's* record of what the fd refers to, not the calling process's
current root, so it still finds the real fifo regardless. Ported the
same fix; `docs/design/`'s launch note and `exec_fifo`'s own doc
comment both point at this.

### Ordering matches real runc exactly, including a documented trade-off

Real `runc` applies seccomp "as close to execve as possible... reducing
the amount of syscalls users need to enable in their seccomp profiles"
— i.e. seccomp is applied *before* the exec-fifo wait, despite that
meaning a sufficiently restrictive user-supplied seccomp profile could
itself break the fifo's own `open`/`write` calls. This project matches
that ordering (`identity::apply` -> `seccomp::apply` -> `exec_fifo::
wait_for_start` -> `exec`) for fidelity with the reference
implementation rather than diverging to a theoretically "safer" order
real runc doesn't use — a profile author accepting this trade-off (or
not) is exactly the same situation either runtime puts them in.

## Reporting the *real* container pid across the PID-namespace relay fork

0012's PID-namespace relay-fork wrinkle (the process that calls
`unshare(CLONE_NEWPID)` never joins the new namespace itself; only its
*next* forked child does, becoming pid 1) means the pid `create` needs
to persist in `state.json` — the one `kill`/`delete` must signal — is
not always `create`'s own direct child. Solved with a pipe: `process::
fork` and `process::wait` were split out of the previous single
`fork_and_wait` (kept as a thin wrapper composing both, so nothing
using it changed) specifically so the relay process can fork the
grandchild, write its pid to a pipe, *then* wait for it — reporting the
pid before potentially blocking for a long time, not after. `create`
reads that pid back and is what actually gets persisted, regardless of
whether a relay fork happened.

`create` itself never waits for the container's own process at all:
once setup finishes and the container is blocked on the fifo, `create`
returns immediately. The now-parentless container process gets
reparented to the nearest subreaper/init once `create`'s own process
exits — ordinary Unix backgrounding, no double-fork/`setsid` needed on
this project's own part (though see the real, verified caveat below).

## Two more real, verified findings — not bugs, matching runc/crun/docker exactly

Manually testing the full lifecycle against a real kernel (`create` /
`start` / `kill` / `delete`, a real busybox bundle, deleted after)
surfaced two things worth documenting precisely because they look like
bugs at first glance and are not:

1. **A plain `SIGTERM` to a PID-namespace's own init is silently
   ignored.** `kill <id>` with no signal argument (defaults to
   `SIGTERM`, matching real `runc kill`) reported success, and the
   container kept running. This is documented, deliberate kernel
   behavior (`man 7 pid_namespaces`): "signals which have a default
   action of terminating the process are ignored for the 'init'
   process [of a PID namespace], unless it has established a handler
   for that signal" — busybox `sh` (this test's container command)
   installs no such handler, and neither do most real container
   `ENTRYPOINT`s without explicit signal-handling code, so real
   `docker`/`podman`/`runc` are subject to the exact same thing (a
   well-known real-world gotcha, not specific to this project). `SIGKILL`
   — which nothing can ignore or handle, pid-namespace init included —
   worked immediately. `tests/tests/ocirun_lifecycle.rs` asserts on
   *both* halves of this rather than only testing the case that "looks
   like it should work".
2. **A backgrounded container's inherited stdio can hang an unrelated
   process indefinitely.** The very first manual test run hung for
   what turned out to be exactly the container's own `sleep 30` — its
   inherited stdout fd (never redirected, matching this project's
   already-documented "no console-socket support yet" gap) kept a pipe
   my own interactive shell was reading from open, so the read blocked
   until the container process itself finally exited. `tests/tests/
   ocirun_lifecycle.rs`'s own `ocirun_create` helper explicitly sets
   `Stdio::null()` on `create`'s stdout/stderr/stdin for exactly this
   reason — the backgrounded container inherits *those*, not a
   `Command::output()`-captured pipe, so this test process's own output
   capture doesn't wait on it.

## Verified against a real kernel, then kept as real automated tests

* Unit tests: `exec_fifo` (the two-sided open/write/read exchange, and
  — the specific regression above — that `wait_for_start` still reaches
  the same fifo via its `O_PATH` fd after the original directory entry
  is removed, standing in for what `pivot_root` does to the original
  path), `signal` (name/number parsing for every alias `libc` defines),
  `process::{fork, wait, kill, alive}` (the primitives `fork_and_wait`
  now composes, plus the new ones `create` needed).
* Manually verified end to end first (scratch bundle + state root,
  deleted after; `setsid` needed to keep the backgrounded container out
  of the *test terminal's own* job-control process group — an
  interactive-shell-specific wrinkle, not something an automated test
  running inside `cargo test`'s own short-lived process needs to worry
  about): `create` -> `state` shows `created` with a real pid ->
  `start` -> `state` shows `running` -> `kill` (`SIGTERM`, ignored) ->
  `kill KILL` (works) -> `state` shows `stopped`, `pid: 0` -> `delete`
  -> `state` reports "does not exist"; separately, `delete --force` on
  a running container, and `delete` (no `--force`) on a `created`-but-
  never-`start`ed container.
* **Real, automated, end-to-end tests**
  (`tests/tests/ocirun_lifecycle.rs`, three cases): the exact scenarios
  above, kept in the test suite. `write_bundle`/`busybox_path` (
  previously private to `ocirun_run.rs`) moved to the shared
  `oci-tools-tests` library crate so both files share one
  implementation, matching the project's own "share as much code as
  possible" pillar applied to its own test helpers, not just production
  code.

## What's still not here

* `exec` (running an *additional* process inside an already-running
  container) and lifecycle hooks (`prestart`/`createRuntime`/
  `startContainer`/...).
* Console-socket/PTY support — every lifecycle command still inherits
  whatever stdio its own caller had, which is workable for `run` but a
  real, documented limitation for `create` once its own process exits
  (see the stdio-hang finding above); a real supervisor (`ociman`,
  eventually) needs to handle this properly rather than every caller
  discovering the same footgun by hand.
* The systemd cgroup driver and full multi-action seccomp profiles
  (already flagged in 0015/0016, unaffected by this increment).
