# Design note 0512: `ociman volume reload`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_volume.rs`.

## What this closes

`0361` deferred `podman volume reload` outright as "plugin-driver-
only... this project has no pluggable volume-driver concept at all",
lumped in with a general scope note, without separately checking
whether it's actually a real, faithful no-op in the one state this
project can ever reach. Re-examined directly this time (the same
class of re-examination that just corrected `mount`/`unmount`'s own
mis-deferral in `0511`, though this one turns out to be a genuine
no-op rather than a mis-scoping): real `podman volume reload`
provably does nothing at all, successfully, whenever no volume-
plugin drivers are configured -- which is the *only* state this
project's own volumes can ever be in, since it has no pluggable
volume-driver concept anywhere.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/volumes/reload.go:34-45`: `reload` calls
  `VolumeReload`, then `printReload("Added", report.Added)` /
  `printReload("Removed", report.Removed)`; `printReload` only
  prints anything when its own `values` slice is non-empty.
- `~/git/podman/libpod/runtime_volume_common.go:261-266`
  (`UpdateVolumePlugins`): `for driverName, socket := range
  r.config.Engine.VolumePlugins { ... }` -- with a real, empty map
  (this project's own only reachable state), the loop body never
  runs at all, so `added`/`removed` both stay `nil`.
- `~/git/podman/cmd/podman/utils/error.go:17-26`
  (`OutputErrors.PrintErrors`): `if len(o) == 0 { return lastError
  }` (`lastError` still its zero value, `nil`) -- an empty error
  slice returns no error either.

So in the one reachable state, `reload()` returns `nil`, prints
nothing, and exits `0` -- a real, checked-directly no-op, not an
assumption.

## Implementation

New `VolumeCommand::Reload` (a bare unit variant, no fields at all --
matching real podman's own `Args: validate.NoArgs`, checked directly,
`reload.go:19`). `cmd_volume_reload` is a one-line `Ok(())`.

Also fixed a real, previously-stale doc comment found while touching
this area: the `VolumeCommand` enum's own module-level doc comment
still claimed `mount`/`unmount`/`reload` were "out of scope for now"
-- never updated after `0361` actually implemented `mount`/`unmount`
over a hundred increments ago.

## Tests

Two new integration tests in `tests/tests/ociman_volume.rs`:
- `volume_reload_is_a_real_no_op_that_prints_nothing` -- succeeds
  with empty stdout/stderr both with no volumes at all and with an
  existing volume present, which survives fully untouched afterward
  (confirmed via its own `mountpoint` directory and a follow-up
  `volume ls -q`).
- `volume_reload_rejects_any_argument` -- matching real podman's own
  `validate.NoArgs`.

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (122
test-result blocks, 0 failures -- no new test file added, so the
block count is unchanged from `0511`; clean on the first attempt
with `RUST_TEST_THREADS=2`), `python3 ci/guards.py` (clean), `cargo
deny check` (clean), `bash ci/native-ci.sh` (clean on the first
attempt with `RUST_TEST_THREADS=2`), `bash ci/build-deb.sh` (clean
on the first attempt, real `dpkg -i`/`--version`/`dpkg -r` round
trip). A pure, always-inert no-op addition -- no hot path touched,
no `ci/bench.sh` rerun needed.

## Deliberately still out of scope

Real podman's own richer plugin-driver machinery (`VolumePlugins`
configuration, actual plugin-socket communication) remains entirely
unimplemented -- this command's own correctness rests specifically
on this project never having any such plugin configured in the
first place, not on faithfully reproducing the plugin-communication
logic itself.
</content>
