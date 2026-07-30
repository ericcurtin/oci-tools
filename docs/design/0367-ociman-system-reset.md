# Design note 0367: `ociman system reset`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_system_reset.rs`,
`README.md`.

## What this closes

`SystemCommand`'s own doc comment (`0263`) named `reset` among real
podman's subcommands "out of scope for now" alongside
`connection`/`events`/`migrate`/`renumber`/`service` — but unlike
those (no daemon, no remote API, no lock-numbering/storage-migration
concept at all here), `reset` is genuinely implementable: this
project already has every removal primitive it needs.

## Real, checked-directly semantics

Read `~/git/podman/libpod/reset.go`'s own `Runtime::Reset` directly:
force-removes every pod (n/a here, no pod concept), every container
(`RemoveContainerAndDependencies(ctx, c, force: true, ...)` —
regardless of status, a running container is never a special case),
every volume, then every image (`Filters: ["readonly=false"]`, force),
then deletes its own `graphRoot`/`runRoot` directories entirely. The
CLI layer (`cmd/podman/system/reset.go`) prints an interactive warning
before confirming (skippable with `-f`/`--force`), and nothing at all
on success.

## A real, deliberate scope divergence: shared storage root

Real podman's own `graphRoot`/`runRoot` are exclusively its own —
wiping them can't affect any other, unrelated tool. This project's
storage root is different by design: `ociman`/`ocirun`/`ocicri`/
`ocibox`/`ociboot` all share **one** root (`oci_cli_common::storage::
default_root()`), this project's own established "one root, not five"
choice for maximizing shared code/disk reuse. A literal port (`rm -rf`
the whole root) would silently destroy `ocibox`'s own `boxes/` and
`ocicri`'s own `cri-containers/`/`cri-sandboxes/`/`cri-bundles/` too —
a genuinely different tool's own separate state that just happens to
share a disk, which `ociman system reset` has no business touching.

`ociman system reset` therefore only ever clears what `ociman` itself
actually owns: `images`/`blobs` (via `oci_store::Store`), `containers`,
`volumes`, `rootfs-cache`, `build-scratch` — every other subdirectory
under the shared root is left completely untouched. Verified directly
by seeding fake sibling-binary directories in the same root and
confirming they survive a real reset call unchanged.

## Implementation

New `SystemCommand::Reset { force: bool }` (`--force`/`-f` accepted for
CLI compatibility, a real no-op — this project has no interactive
confirmation prompt anywhere to skip in the first place, the same
reasoning `ContainerCommand::Prune::force` already established).
`cmd_system_reset`: force-removes every container (any status, reusing
`remove_container(.., force: true, ..)`, the same primitive `ociman
rm`/`container prune` already use) *before* volumes and images, so
neither is ever still "in use" by the time each is reached — the same
container-before-image ordering `0358` already established for
`ociman prune`. Removes every volume (`VolumeStore::remove`, no "in
use" check needed at all by this point), then every image
(`Store::remove_image`) plus a blob GC and rootfs-cache prune (reusing
the exact same calls `cmd_prune`/`prune_images_and_reclaim` already
make). Finally wipes the entire `build-scratch` directory
unconditionally — unlike `ociman prune`'s own age-gated
`prune_build_scratch` pass, matching real podman's own "all build
cache" reset scope exactly (nothing is worth keeping once a reset is
requested, fresh or not).

Prints nothing on success, matching a real installed `podman system
reset -f`'s own checked-directly identical silent completion.

## Verified

New `tests/tests/ociman_system_reset.rs`:
`reset_on_an_empty_store_succeeds_silently`;
`reset_removes_every_container_volume_and_image_regardless_of_status`
(a real stopped container, a real *genuinely running* one, a volume,
and an image, all confirmed present before reset and gone after —
proving a live container is never exempted, matching real podman's
own identical force-removal of everything);
`reset_never_touches_a_sibling_binarys_own_storage` (seeded
`boxes/`/`cri-containers/`/`cri-sandboxes/`/`cri-bundles/` marker
files survive untouched);
`reset_force_is_accepted_and_behaves_identically`.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures, full clean
run; one transient, already-documented `ocicri_container.rs`
`exec_sync_runs_commands_in_a_running_container` flake on the first
full-suite/`ci/native-ci.sh` run, confirmed transient via an isolated
`--test-threads=1` rerun and a second clean full `native-ci.sh` run),
`python3 ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh`,
`bash ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg -r` round
trip).
