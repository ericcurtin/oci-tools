# Design note 0362: `ociman mount`/`ociman unmount`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_mount.rs`,
`README.md`.

## What this closes

Real `podman mount CONTAINER`/`podman unmount CONTAINER` (and their
`podman container mount`/`unmount` aliases) had no equivalent here at
all — flagged as a candidate while wrapping up `0361`'s own identical
`ociman volume mount`/`unmount`.

## Why these are flat, top-level commands, not nested under `ContainerCommand`

`Command::Container`'s own doc comment (`0287`) is explicit:
`ContainerCommand` exists *only* to host a real podman subcommand with
**no** flat top-level alias (`exists`, later `prune`, `0357`). Real
`podman mount`/`unmount`, unlike those two, genuinely *do* have a bare
top-level alias in real podman (`~/git/podman/cmd/podman/containers/
mount.go`'s own `mountCommand`, registered with no `Parent`, sharing
the identical `RunE` its own `containerMountCommand` nested variant
also uses) — so, per this project's own already-stated convention,
these land as flat `Command::Mount`/`Command::Unmount`, matching every
other container verb (`ps`/`rm`/`inspect`/...).

## Real, checked-directly semantics

Traced `~/git/podman/libpod/volume_internal.go`'s `needsMount()`-style
reasoning already established for `0361`, this time for containers:
real podman's own storage layer genuinely needs a real mount/unmount
operation (overlay2/vfs/fuse-overlayfs, real mount-count refcounting,
`~/git/podman/cmd/podman/containers/unmount.go`'s own doc string) —
this project's own containers never do. A container's own root
filesystem is already extracted to a real, directly-accessible
directory the moment it's created
([`oci_runtime_core::PersistedState::rootfs`]) — there is no separate
"mount" step to actually perform at all, mirroring `0361`'s own
identical reasoning for volumes.

`unmount` is therefore a real no-op unconditionally — checked directly
against a real installed `podman unmount` too (rootful; rootless needs
no `podman unshare` for this one, unlike `mount`, matching
`~/git/podman/cmd/podman/containers/unmount.go`'s own annotations,
which name no `UnshareNSRequired` at all).

`mount` does share one real, already-established gap: a container
using this project's own rootless-overlay rootfs optimization
(`0110`) has its own real writes land in a private `upper/` directory
this project has no whiteout-aware merge logic for yet — the exact
same gap `cp`/`diff`/`export`/`commit` already have, via the shared
[`resolve_container_root`] helper, reused verbatim rather than
reimplemented. `unmount` has no such gap at all (it never needs to
read the container's own real merged view), so it's implemented
without going through [`resolve_container_root`] — only a plain
existence check.

Real podman also supports a bare `podman mount`/`podman unmount --all`
(no `CONTAINER` at all, listing/unmounting every currently-mounted
one) — deliberately deferred for this first slice, the same narrower-
first-slice precedent `ContainerCommand::Prune`'s own deferred
`--filter` already used.

## Verified

New `tests/tests/ociman_mount.rs` (mirrors `ociman_diff.rs`'s own
already-established "force `.rootless-overlay-supported` to `false`"
convention and its "test passes either way this host lands" technique
for the one overlay-specific test):
`mount_prints_the_real_rootfs_path_of_a_stopped_container`;
`mount_works_on_a_genuinely_running_container_too`;
`unmount_is_a_real_no_op_that_prints_the_container_id` (asserts the
container's own rootfs survives fully intact afterward);
`mount_and_unmount_against_an_unknown_container_are_clear_errors`;
`mount_is_a_clear_error_for_a_rootless_overlay_rootfs_container_but_
unmount_still_succeeds` (the one test proving the real asymmetry
between the two: `mount` refuses clearly on this host if it happens to
support the optimization, `unmount` never does regardless).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures, full clean
run, no flakes), `python3 ci/guards.py`, `cargo deny check`, `bash
ci/native-ci.sh`, `bash ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).
