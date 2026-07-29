# Design note 0277: `ocirun kill -a`/`--all`

Status: implemented
Scope: `bin/ocirun/src/main.rs`, `tests/tests/ocirun_lifecycle.rs`.

## Closing a real, purely compositional CLI-compatibility gap

Real `crun kill --help` (checked directly against a real installed
`crun`) has `-a`/`--all`: "kill all the processes" — sends the given
signal to every process in the container's own cgroup, not just its
recorded init pid. Real `runc kill --help`, checked directly, has no
equivalent flag at all (its own help text lists no options beyond the
positional `container-id [signal]`). This closes the `crun`-side gap.

## Real semantics, checked directly against `~/git/crun/src/kill.c`/
`libcrun/cgroup-utils.c`

`crun`'s own `-a`/`--all` calls `libcrun_container_killall`, which
calls `cgroup_killall_path`. That function's own real sequence:

1. If the signal is `SIGKILL` specifically, try a single atomic write
   to the real cgroup v2 `cgroup.kill` file first (a real kernel
   feature that kills every process in the cgroup in one step, no
   races possible) — falls through to the steps below if that write
   fails.
2. Freeze the cgroup (pause) — so a process forking a new child mid-
   sweep can't dodge the signal by escaping into a not-yet-listed pid.
3. Read every real pid currently in the cgroup.
4. Signal each one (`ESRCH`, already gone, silently tolerated — a real
   race between listing and signaling is expected and harmless).
5. Unfreeze the cgroup (thaw) again, unconditionally.

This project's own implementation matches steps 2–5 exactly, reusing
already-tested primitives with **zero new engineering**: `oci_runtime
_core::cgroups::set_frozen`/`all_pids` (already used by `ocirun pause`/
`resume`/`ps`) and `oci_runtime_core::process::kill` (already used by
the existing single-pid `kill`). Step 1's own `cgroup.kill`-file fast
path was deliberately **not** ported: it's a pure optimization crun
uses only for the `SIGKILL` case, functionally identical to (just
faster than) the freeze/sweep/thaw sequence already implemented for
every other signal — not a behavioral difference worth a second,
parallel code path for.

The cgroup is always unfrozen again before returning, even if listing
pids or an individual `kill(2)` call failed partway through — a `--all`
call must never leave a container's own cgroup stuck frozen behind it.

## Verified

Integration (`tests/tests/ocirun_lifecycle.rs`, one new test):
`ocirun kill --all <id> KILL` terminates a real running container
(confirmed via `wait_for_status` reaching `stopped`), and — the one
correctness property that matters most for this specific new code
path — the real `cgroup.freeze` file reads back `"0"` afterward,
confirmed by reading it directly (the same technique the existing
pause/resume test already uses): the freeze/thaw cycle around the
kill sweep never leaves the cgroup stuck frozen.

An earlier, more ambitious draft of this test tried to demonstrate
`--all`'s own value-add directly, by killing a background child
process a multi-process container's init had spawned via a plain
(non-`--all`) `kill` first. That draft's own assumption — that a
container's pid-namespace init ignores an unhandled-default-action
signal like `SIGTERM` (real, documented `pid_namespaces(7)` kernel
behavior, and the same reasoning the existing `create_start_kill_
delete_lifecycle` test already relies on for a *single*-process
container) — turned out not to hold cleanly once a background child
and the cgroup freeze/thaw cycle were combined in the same test,
empirically observed to sometimes terminate init anyway rather than
leaving it running. Rather than ship a test asserting a specific
process/signal interaction not fully, confidently understood, the
test was simplified to the single, most important, and unambiguous
correctness property instead: the freeze/thaw cycle itself.

Full workspace: `cargo build`/`test --workspace`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`python3 ci/guards.py`, `cargo deny check`, `ci/native-ci.sh`,
`ci/build-deb.sh`.

## Still ahead

The multi-process pid-namespace-signal-immunity interaction noted
above remains a real, not-fully-understood edge case worth its own
dedicated, careful investigation in a future increment, separate from
this note's own narrower, successfully-verified scope.
</content>
</invoke>
