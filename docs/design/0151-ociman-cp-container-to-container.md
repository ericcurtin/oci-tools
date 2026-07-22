# Design note 0151: `ociman cp` container-to-container

Status: implemented
Scope: `bin/ociman/src/main.rs` (`cmd_cp`'s new `(Some, Some)` match
arm, doc comment updated); `tests/tests/ociman_cp.rs` (new
`seed_and_run_named_stopped_container` helper, 2 new tests replacing
the old "is a clear error" one).

## Closing the gap 0146 explicitly deferred

0146's own "what this doesn't do yet" named this directly:
container-to-container copying (real `podman cp` supports it,
`~/git/podman/cmd/podman/containers/cp.go`'s own
`copyContainerToContainer`, streaming a tar archive between the two
containers over an `io.Pipe` internally) wasn't supported yet — this
project's own first `cp` increment only covered the far more common
"one side is the host" case.

## Almost the entire feature already existed

Real podman's own implementation needs a real pipe and two concurrent
goroutines specifically because its own two containers' storage might
not even be reachable from the same process without going through its
own API layer (or, in the remote/podman-machine case, isn't even on
the same real filesystem at all). Neither applies here: both
containers' own storage already lives directly on the very same local
filesystem this same `ociman` process already has ordinary, synchronous
`std::fs` access to — so container-to-container `cp` needed no new
copying logic of its own at all. `cmd_cp`'s pre-existing `(Some,
None)`/`(None, Some)` arms already did exactly the resolve-container-
root-then-`copy_cp_path` sequence a `(Some, Some)` arm needs, just
once each instead of twice: the entire real change is one new match
arm calling [`resolve_container_root`] a second time and passing both
resolved paths into the exact same, already-tested [`copy_cp_path`].

`resolve_container_root`'s own rootless-overlay-rootfs rejection (the
one real, still-standing gap this feature shares with the host-sided
case) is checked independently for *each* container named — so, for
example, a container-to-container copy where only the *destination*
happens to use the optimization still fails with a clear error rather
than silently succeeding while actually writing into the wrong (empty,
on the host's own view) directory.

## Real, automated tests

Two new integration tests replace the old "container-to-container is
a clear error" one (removed, since the behavior it asserted is exactly
what this increment intentionally changes): a real file copied
directly from one container's own storage into another's (with the
source container's own copy confirmed untouched afterward — this is a
copy, not a move); an unknown destination container being a clear
error. A new `seed_and_run_named_stopped_container` test helper was
needed alongside the pre-existing `seed_and_run_stopped_container`:
the older helper resolves its own just-created container via `ps -a
-q`, which lists *every* container in the storage root — fine when
only one container ever exists in it at a time (every pre-existing
test here), but genuinely ambiguous the moment a second container
needs to coexist in the same store, exactly what a real container-to-
container test needs. The new helper sidesteps this with an explicit
`--name` up front instead, avoiding the `ps`-based lookup (and its own
inherent ambiguity once more than one container exists) entirely —
found by running the new test once as first written and seeing it
fail with a bogus multi-line "container id" (two real ids
newline-joined by the ambiguous `ps -a -q` call), not guessed at
before running.

Full `cargo build --workspace --locked`/`cargo test --workspace
--locked` (2 clean runs)/`cargo fmt --all --check`/`cargo clippy
--workspace --all-targets --locked -- -D warnings`/`python3
ci/guards.py`/`cargo deny check`/`bash ci/native-ci.sh` all clean.

## What this doesn't do yet

* **Rootless-overlay-rootfs containers** — same real, standing gap
  0146 already has on either side of a copy; needs real
  overlayfs-whiteout-aware directory merging this project doesn't
  implement yet.
* This project has no remote/network transport for container storage
  at all (unlike real podman's own remote-client mode), so there was
  never a need to replicate podman's own pipe-based streaming
  mechanism here — this is a deliberate simplification specific to
  this project's own architecture, not a narrowed subset of the
  container-to-container case itself.
