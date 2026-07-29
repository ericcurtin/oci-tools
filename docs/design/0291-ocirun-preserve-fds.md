# Design note 0291: `ocirun run`/`create --preserve-fds`, and closing a real, previously-latent fd-leak gap

Status: implemented
Scope: `crates/oci-runtime-core/src/launch.rs`, `bin/ocirun/src/main.rs`,
`bin/ocibox/src/main.rs`, `bin/ocicri/src/launcher.rs`,
`bin/ociman/src/build.rs`, `bin/ociman/src/main.rs`,
`bin/ocivmm/src/main.rs`, `tests/tests/ocirun_run.rs`.

## A real, checked-directly gap in both reference runtimes

`~/git/runc/utils_linux.go`/`~/git/crun/src/run.c` both have
`--preserve-fds N`: "pass `N` additional file descriptors to the
container" (real socket-activation-style plumbing for a supervisor
like `containerd`/`systemd` that already has extra fds open at fd
3.. before invoking the runtime). Confirmed directly: `ocirun run`/
`create --help` had no such flag at all.

## A real, more serious gap found while implementing it: no default fd-closing step existed at all

Real runc (`utils_linux.go`'s own `baseFd := 3 + len(process.
ExtraFiles)`, `unix.Faccessat` loop) and real crun
(`libcrun/container.c`'s `mark_or_close_fds_ge_than`, called with
`context->preserve_fds + 3` right before the container's own process
ever execs) both **always** close every fd above stdio (+ whatever
`--preserve-fds` explicitly keeps) as part of their own default launch
sequence — this is required by the OCI runtime's own implicit
contract (a container process should never see an fd it wasn't meant
to), not something `--preserve-fds` merely adds on top of.

`oci_runtime_core::launch`'s own `mount_pivot_and_exec` had **no such
step at all**, for any container, before this change. Since the final
container process launch uses `std::process::Command::exec()` (a real
`execve()`, not a fork+exec through a shell), any fd this project's
own caller happened to have open beyond stdio — inherited, non-
`CLOEXEC`-marked, however it got there — would have leaked straight
into *every* container this engine ever started, completely
unconditionally, regardless of any flag. This is a real, previously-
unnoticed correctness/isolation gap, found while researching what
`--preserve-fds` actually needs to do, not merely a missing flag.

## Implementation

- `close_fds_ge_than(first_fd)`: a single `close_range(2)` syscall
  (Linux 5.9+, glibc 2.34+) via `libc::close_range` directly — no
  legacy `/proc/self/fd`-iteration fallback, the same "assume a modern
  kernel" precedent `user_resolve.rs`'s own `openat2(2)` use already
  established (this project's own two first-class target distros,
  CentOS Stream 10 and Ubuntu 26.04, are both comfortably new enough).
- Called from a `pre_exec` closure registered on the final
  `std::process::Command`, not a plain call before it — verified
  directly against `std`'s own `do_exec` source
  (`library/std/src/sys/process/unix/unix.rs`): `pre_exec` closures
  run *after* `Command`'s own internal stdio `dup2`s but *before* the
  real `execve`, which matters here specifically because the raw
  source fds behind `stdin_fd`/`stdout_log_fd`/`stderr_log_fd` are
  themselves ordinary fds `>= 3` — closing them *before* `Command` had
  a chance to `dup2` them onto 0/1/2 would have broken every existing
  stdio-redirection caller (`ociman run --log`, `ociman build -q`,
  `ociman build`'s stdin-closing) outright.
- `ChildSetup` gained one new `preserve_fds: u32` field (default `0`
  from `build_child_setup`); `run`/`run_reporting_pid`/`create` each
  gained a matching new parameter, following this project's own
  already-established precedent for adding a narrow new knob to these
  same three heavily-shared functions (`close_stdin`/`discard_output`,
  0187/0196) — every existing call site across `ocibox`/`ocicri`/
  `ociman` (×2)/`ocivmm` updated to pass `0` (none of those five real
  equivalents — `distrobox`, `cri-o`, `podman`, `docker`, and this
  project's own `ocivmm` — expose an equivalent flag of their own);
  only `ocirun`'s own CLI threads a real value through.
- `ocirun run`/`create --preserve-fds N`: fails fast, before ever
  forking a container at all, if fewer than `N` fds are actually open
  starting at fd 3 (`verify_preserve_fds`, an `/proc/self/fd/<fd>`
  existence check) — matching real runc's own identical upfront
  `Faccessat` check exactly, rather than silently succeeding with
  fewer fds than claimed or failing confusingly deep inside the
  container's own process instead.
- Scope: `run`/`create` only, not `exec` (a different, separate code
  path in this crate entirely — real runc/crun's own `exec
  --preserve-fds` too) — a real, deliberately deferred candidate.

## Verified

Manual, end-to-end (real `ocirun spec --rootless` bundle, a real bash
`3>file` redirect):

- Without `--preserve-fds`: the container's own `/proc/self/fd`
  listing shows only 0/1/2 — a real fd 3 the caller had open is
  genuinely closed, confirming the previously-latent leak this closes.
- With `--preserve-fds 1`: fd 3 is present inside the container,
  pointing at the exact file the caller had open there.
- `--preserve-fds 5` with only stdio open fails immediately with a
  clear, actionable error naming the missing fd — not a bare crash, a
  container that half-starts, or a silent under-delivery.

Integration (`tests/tests/ocirun_run.rs`, two new tests): the same two
end-to-end cases, using a `pre_exec` closure on the *test's own*
spawned `ocirun run` subprocess to `dup2` a real, already-open file
onto exactly fd 3 in that one child. A real, easy-to-hit POSIX gotcha
found and fixed while writing this test, not merely inspected: `dup2
(3, 3)` (when the source fd already happens to equal 3, a genuine
possibility depending on whatever the test harness itself already has
open) is specified to be a no-op that leaves `FD_CLOEXEC` untouched —
unlike an ordinary `dup2` to a *different* target, which always clears
it on the new descriptor — so a `tempfile`-crate-opened source fd
(commonly `O_CLOEXEC` by default) could silently vanish at the test's
own `execve()` into `ocirun`, before `ocirun` ever got a chance to see
it, indistinguishable from `--preserve-fds` genuinely not working.
Fixed by unconditionally clearing `FD_CLOEXEC` on fd 3 after the
`dup2`, regardless of which case applies.

Performance (this touches the hot container-launch path,
`mount_pivot_and_exec`, on *every* container start, not just
`--preserve-fds` callers): re-ran `ci/bench.sh`'s `ocirun run`/`ocirun
exec` sections after this change — both unchanged within ordinary
session noise (`ocirun run` 3.2ms vs. `0288`'s 3.1ms baseline,
`ocirun exec` 1.9ms vs. `0288`'s 2.0ms) — a single `close_range(2)`
syscall is effectively free relative to the rest of container
startup (namespace creation, mounts).

Full workspace: `cargo build`/`test --workspace` (111 test binaries),
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets --
-D warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh`.

## Still ahead

`ocirun exec --preserve-fds` (a separate code path, real runc/crun
both also support it there).
