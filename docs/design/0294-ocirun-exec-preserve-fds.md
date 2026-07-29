# Design note 0294: `ocirun exec --preserve-fds`

Status: implemented
Scope: `crates/oci-runtime-core/src/exec.rs`,
`crates/oci-runtime-core/src/process.rs`,
`crates/oci-runtime-core/src/launch.rs`, `bin/ocirun/src/main.rs`,
`bin/ociman/src/main.rs`, `bin/ocicri/src/launcher.rs`,
`tests/tests/ocirun_exec.rs`.

## Closing `0291`'s own "still ahead"

`0291` implemented `ocirun run`/`create --preserve-fds` and, more
importantly, closed a real, previously-missing default fd-closing step
in `crate::launch`'s own container-launch sequence. Its own "still
ahead" named the obvious next candidate directly: `ocirun exec` is a
genuinely separate code path (`crate::exec`, joining an already-running
container's namespaces rather than creating fresh ones) that never got
the same treatment — both real `runc exec`/`crun exec` support
`--preserve-fds` identically to their own `run`/`create`, and
`crate::exec`'s own final `exec_now` had exactly the same missing
default-close gap `launch.rs` did, independently.

## Sharing the fix without duplicating it

`close_fds_ge_than` (the single `close_range(2)` syscall wrapper 0291
introduced) moved out of `launch.rs` into `crate::process` — the
crate's own existing home for raw, `libc`-level process primitives
(`fork`/`kill`/`wait`), a more natural fit than either `launch`- or
`exec`-module-private duplication would have been, matching this
project's own "one implementation per function" pillar. Both
`launch.rs` and `exec.rs` now call the identical shared function from
their own `pre_exec` closures, at the identical point in their own
respective `Command` setup (after any stdio redirection is registered,
before the real `execve`).

`ExecRequest` gained one new `preserve_fds: u32` field (default `0`),
following the exact same pattern `0291` established for `ChildSetup`/
`run`/`create`. Every existing caller of `oci_runtime_core::exec::exec`
updated to pass `0`: `ociman exec` (matching real `podman exec`'s own
checked-directly lack of an equivalent flag, `0276`), `ociman
healthcheck run`, and `ocicri`'s `ExecSync` (real CRI's own
`ExecSyncRequest` has no equivalent field at all). Only `ocirun exec`'s
own CLI threads a real value through, with the identical upfront
`verify_preserve_fds` fail-fast check `0291` already added for `run`/
`create`, reused verbatim (unmodified — it only needs `n`, not which
subcommand is calling it).

## Verified

Manual, end-to-end (a real `create`+`start` container, `ocirun exec`
into it): without `--preserve-fds`, the exec'd process's own
`/proc/self/fd` listing shows only 0/1/2 — a real fd 3 the caller had
open is genuinely closed. With `--preserve-fds 1`, fd 3 is present,
pointing at the exact file the caller had open there.

Integration (`tests/tests/ocirun_exec.rs`, two new tests, run
repeatedly to confirm no flakiness): the same two end-to-end cases via
a real `create`+`start`+`exec` lifecycle, using the identical
`pre_exec`-based `dup2`-onto-fd-3 technique (and the identical
`FD_CLOEXEC`-clearing fix for the `dup2(fd, fd)`-is-a-no-op gotcha)
`ocirun_run.rs`'s own `--preserve-fds` test already established for
`run`; a `--preserve-fds 5` claim with only stdio open fails
immediately with the same clear, actionable error `run`/`create`
already give.

Performance (this touches `crate::exec`'s own hot path — `ocirun
exec`, `ociman exec`, and `ocicri`'s `ExecSync` kubelet liveness/
readiness probes all route through it): re-ran `ci/bench.sh`'s
`ocirun exec`/`ociman exec` sections after this change — both
unchanged within ordinary session noise (`ocirun exec` 1.9ms, `ociman
exec` 2.8ms) — a single `close_range(2)` syscall is effectively free
relative to the rest of the exec path (namespace join, identity drop).

Regression: all 8 pre-existing `ocirun_exec.rs` tests still pass
unmodified.

Full workspace: `cargo build`/`test --workspace` (111 test binaries),
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets --
-D warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh`.

## Still ahead

None remaining for `--preserve-fds` itself — both real runc/crun
subcommand pairs that support it (`run`/`create` and `exec`) now have
a matching, shared implementation.
