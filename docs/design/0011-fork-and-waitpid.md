# Design note 0011: `fork(2)`/`waitpid(2)` primitive (milestone 3, part 9)

Status: implemented (ninth increment of milestone 3)
Scope: `oci_runtime_core::process`. Still no CLI wiring — this is the
last *primitive* `create`/`run` needs; assembling all of 0003–0011 into
an actual `ocirun` command is the next (and now final-primitive-free)
increment.

Continues 0003–0010. Every earlier increment built a piece of *what* a
container needs (namespaces, mounts, cgroups, the setup sequence);
nothing so far actually creates the container *process*. That needs
`fork(2)`, which — deliberately — none of this workspace's chosen
libraries provide.

## Why `libc`, and why that's not a second "low-level unix syscalls" pick

`rustix` (this workspace's choice for that capability group) omits raw
`fork()` on purpose: a forked child inherits only the calling thread, so
in a multithreaded parent, any lock a *different* thread held at the
moment of `fork()` — the allocator's, a `Mutex`'s — stays locked forever
in the child, which never runs the thread that would release it. Only
async-signal-safe operations are guaranteed sound in the child until it
calls `exec`/`_exit`. Rather than expose that footgun, `rustix` leaves
`fork()` out entirely (confirmed by searching its source — no `fork` of
any kind, safe or unsafe, appears anywhere in the crate).

So `libc` is added as a new, separate, direct dependency — not listed
under the `ci/guards.py` "low-level unix syscalls" capability group
alongside `rustix`, and deliberately so: that guard's job is stopping
two *competing* picks for the same capability (e.g. both `tokio` and
`async-std`), and `libc` isn't competing with `rustix` here, it's filling
the one gap `rustix` leaves on purpose. Adding it to that group's
alternatives list would make the guard fail on exactly the combination
this design intends.

## `oci_runtime_core::process`

* `fork_and_wait(child_body: impl FnOnce()) -> io::Result<i32>` — forks;
  the child runs `child_body` (expected to end in `exec`/`_exit`/
  `std::process::exit`, never return) and calls `_exit(127)` itself if
  `child_body` returns anyway, so a forked child can never fall back into
  the parent's own subsequent code; the parent `waitpid`s (retrying on
  `EINTR`) and returns the raw status. `unsafe` — the function's doc
  comment states the same async-signal-safety contract `fork(2)` always
  has, so a caller reads it as an obligation on what `child_body` may do,
  not just a formality.
* `exit_code_from_wait_status(status: i32) -> i32` — decodes a raw
  `waitpid(2)` status the way a shell (and every other OCI runtime CLI)
  reports a process's exit code: the real exit code if it exited
  normally, `128 + signal` if a signal killed it.
* The whole module is `#![allow(unsafe_code)]` at the top rather than
  annotating every individual call site — its entire purpose is wrapping
  three raw FFI calls (`fork`/`_exit`/`waitpid`), unlike the rest of the
  workspace (which stays `unsafe`-free apart from the few individually-
  justified sites in `namespaces`/`oci_mount::syscalls`) — matching how
  `rustix` itself annotates its own raw-syscall modules.

## Real automated tests, not manual scratch verification

Unlike `unshare(CLONE_NEWUSER)` (0006) or an actual `mount(2)`/
`pivot_root(2)` call (0008), plain `fork()` has no "calling process must
be single-threaded" restriction — that restriction is specific to
`CLONE_NEWUSER`. The only hazard is what the *child* does before
`exec`/`_exit`, and a test child that immediately calls `_exit(N)` touches
no shared state at all, so it's sound to fork for real inside an ordinary
`#[test]`, even in `cargo test`'s multithreaded harness. Four tests do
exactly that: a specific exit code, success, a sweep of several distinct
codes (each getting its own independent forked child), and the
signal-exit-code math (`128 + signal`) checked without needing to
actually send a signal (constructing the same low-order-bits encoding
`WIFSIGNALED`/`WTERMSIG` expect and asserting the decode matches).

## What this unlocks for the next increment

`create`/`run` can now: compute `clone_flags_for(...)` (0006), `fork_and_
wait` a child that `unshare`s (0006), writes its own ID mapping (0006),
executes `plan_rootfs_setup`'s actions via `oci_mount::syscalls` (0008),
applies cgroup resource limits (0009 — modulo the cgroup-directory-
creation/process-migration piece 0009 didn't cover), and finally
`std::os::unix::process::CommandExt::exec`s the container's process,
while the parent waits and reports the same exit code the container's
process itself would have produced to its own shell.

An important test-design consequence worth flagging now, for whoever
picks up the next increment: a *real* end-to-end `ocirun run` integration
test — unlike `unshare` calls made directly inside a `#[test]` body — is
also *not* blocked by the single-thread restriction, because
`tests/tests/*.rs` integration tests spawn the compiled `ocirun` binary
as a subprocess (`Command::new(bin_path("ocirun")).output()`, the same
pattern `ocirun_state.rs`/`ocirun_spec.rs` already use). That subprocess
starts fresh and single-threaded from its own `main()`, regardless of how
many threads the test harness itself has — so `ocirun run`'s own
`unshare(CLONE_NEWUSER)` call, made early in that fresh process, is sound
and testable for real, not just manually. Nothing needs to change to make
that true; it's a property of how the existing integration tests already
invoke the built binaries, worth designing the next increment's tests
around rather than reaching for another manual scratch program.

## Decisions and risks

* No CLI wiring yet: `ocirun run`/`create`/`start` remain unimplemented.
  Everything 0003–0011 built is now a complete parts list; assembling it
  is deliberately left as its own increment given its size and the two
  known gaps 0010 already flagged (rootless `/sys` read-only remount,
  cgroup-v1-vs-v2 mount-type auto-detection).
