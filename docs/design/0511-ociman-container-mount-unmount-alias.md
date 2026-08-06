# Design note 0511: `ociman container mount`/`unmount` aliases

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_container.rs`.

## What this closes

`0510` had labeled `mount`/`unmount` as "cross-concept aliasing,
unverified" in its own "Remaining, explicitly NOT well-scoped" list,
without actually checking real podman's own source for these two
commands. Re-examined directly this time (the same "re-examine an
old deferral against exact upstream source" technique `0499`/`0509`/
`0510` all already used successfully): `mount`/`unmount` turn out to
be genuinely tractable, the exact same byte-identical-alias shape
already established for all 21 other `ociman container <verb>`
members (`0488`-`0507`) -- the earlier "cross-concept" label was
simply wrong.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/mount.go:41-48`:
  `containerMountCommand` (`Parent: containerCmd`) copies
  `mountCommand`'s own `Use`/`Short`/`Long`/`RunE`/`Args`/
  `ValidArgsFunction` verbatim.
- `~/git/podman/cmd/podman/containers/mount.go:54-64` (`mountFlags`):
  the identical flag set (`--all`/`-a`, `--format`, `--no-trunc`)
  applied to both registrations via the one shared function, plus
  `--latest` via `validate.AddLatestFlag` on both.
- `~/git/podman/cmd/podman/containers/unmount.go:38-51`:
  `containerUnmountCommand` (`Parent: containerCmd`) copies
  `unmountCommand`'s own `Use`/`Short`/`Aliases` (`["umount"]`)/
  `Long`/`RunE`/`Args`/`ValidArgsFunction` verbatim -- including the
  `umount` alias itself on *both* registrations.
- `~/git/podman/cmd/podman/containers/unmount.go:57-59`
  (`unmountFlags`): the identical flag set (`--all`/`-a`, `--force`/
  `-f`) applied to both, plus `--latest` via
  `validate.AddLatestFlag` on both.

## Implementation

`ContainerCommand` gains `Mount { containers, all, latest }` and
`Unmount { containers, all, latest, force }` (the latter with
`#[command(alias = "umount")]`, matching real podman's own nested
`umount` alias). Both dispatch straight into the already-existing
`cmd_mount`/`cmd_unmount` `ociman mount`/`ociman unmount`
themselves already call, with the identical field set -- the same
"raw fields straight through" dispatch shape `Rm`/`Stop`/`Kill`/etc.
already established, since `Command::Mount`/`Command::Unmount`'s own
top-level dispatch arms do their own validation inside `cmd_mount`/
`cmd_unmount` rather than inline. `force` is destructured and
discarded (`force: _`) exactly as the top-level `Command::Unmount`
arm already does, since it's a real, checked-directly no-op there
too.

This project's own `Command::Mount` is honestly narrower than real
podman's own `mount` (no `--format`/`--no-trunc` yet, a pre-existing
gap unrelated to this increment) -- the alias faithfully mirrors
that same narrower scope rather than inventing a wider one.

## Tests

Two new integration tests in `tests/tests/ociman_container.rs`:
- `container_mount_is_a_byte_identical_alias_for_top_level_mount` --
  a real, running container's own root path printed identically
  through the alias.
- `container_unmount_is_a_byte_identical_alias_for_top_level_unmount`
  -- the real no-op prints the container's own id through both
  `container unmount` and the nested `umount` alias.

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (122
test-result blocks, 0 failures -- no new test file added, so the
block count is unchanged from `0510`; clean on the first attempt
with `RUST_TEST_THREADS=2`), `python3 ci/guards.py` (clean), `cargo
deny check` (clean), `bash ci/native-ci.sh` (the documented
transient `ocicri_container.rs` flakiness under this host's own
persistent CPU contention showed up across two consecutive
attempts, each time in a different, unrelated test within that same
file, confirmed transient by rerunning each failing test in
isolation three times in a row -- always passed -- then a fully
clean run with `RUST_TEST_THREADS=1` throughout), `bash
ci/build-deb.sh` (clean on the first attempt, real `dpkg -i`/
`--version`/`dpkg -r` round trip). This is pure CLI-dispatch-layer
plumbing onto two already-existing primitives -- no hot path
touched, no `ci/bench.sh` rerun needed.

## Deliberately still out of scope

`port` (no networking subsystem), `init` (a real, separate
lifecycle phase in real podman with no equivalent split here --
re-confirmed directly this time too, `~/git/podman/libpod/
container_internal.go:1025`), and `runlabel` (no top-level
equivalent at all, needs real OCI-label parsing + pull + optional
replace, a genuinely new subsystem) remain the last three
`ociman container <verb>`-family candidates; all three were
independently re-checked this time and confirmed correctly deferred,
not mis-scoped the way `mount`/`unmount` turned out to be.
</content>
