# Design note 0302: `ociman volume export`/`ociman volume import`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_volume.rs`.

## The gap

`docs/design/0173` (named volumes) explicitly deferred real podman's
own `volume export`/`volume import`/`mount`/`unmount`/`reload`
subcommands. `VolumeCommand`'s own doc comment repeated this ever
since, and nothing revisited it. Real `podman volume export`/`podman
volume import` (checked directly against an installed `podman 4.9.3`;
real docker has no equivalent at all) let you back up a named
volume's own content to a plain tar and restore it later, or move it
between hosts — a real, meaningful, previously-flagged gap.

## No new subsystem logic needed

Both subcommands reuse primitives this project already has, fully
tested elsewhere:

- `ociman volume export` is `cmd_export` (container filesystem export,
  `oci_layer::export_tree`) pointed at a volume's own `_data`
  directory instead of a container's rootfs — identical code shape,
  zero new logic.
- `ociman volume import` is `ociman import`'s own "peek two bytes for
  gzip's magic number, else assume a plain tar" convention, feeding
  the result into `oci_layer::apply` (the same primitive `ociman
  run`/`build`'s own base-layer extraction already uses) instead of
  synthesizing a whole new image around it.

## A real semantic check against real podman, not assumed

Read `~/git/podman/libpod/volume.go`'s own `Import` directly rather
than assuming "wipe first, then extract" (a plausible but wrong
guess): real podman's own `Import` is a bare `chrootarchive.Untar`
onto the volume's mountpoint, with **no removal of existing content
first** — a plain tar extraction merges onto whatever's already
there (same-path entries get overwritten; nothing else is touched).
Ported exactly that way: `oci_layer::apply` is likewise a plain
extraction with no prior wipe, so no extra step was needed to match
this.

`ociman volume export`/`import` (like real podman's own versions)
require the named volume to already exist — `LookupVolume` in real
podman's own `VolumeExport`/`VolumeImport`, matched here by
`store.exists(name)` checks giving a clear "no volume with name ...
found: no such volume" error otherwise, consistent with every other
`ociman volume` subcommand's own established error message.

## Deliberate scope narrowing

`ociman volume import` only auto-detects gzip (via its own two-byte
magic number, exactly matching `ociman import`'s own identical
convention); anything else is read as a plain, uncompressed tar.
`oci_layer::apply` itself *can* decode a `zstd` stream (used elsewhere
for real OCI image layers with that media type), but this command
doesn't auto-sniff it from two bytes alone the same way gzip is —
matching `ociman import`'s own already-established scope exactly, not
a new limitation. Real `podman volume import` additionally
auto-detects `bzip2`/`xz`, which no command in this project has ever
supported.

## Verified

Manual, end-to-end: created a volume, wrote nested files into its own
real mountpoint directory, `ociman volume export -o file.tar`,
`ociman volume import` into a fresh volume, confirmed byte-for-byte
content match; verified stdin import (`cat file.tar | ociman volume
import name -`) and gzip-compressed import both work; confirmed
export-to-stdout still works. Confirmed real, bidirectional
cross-tool interoperability against an installed `podman 4.9.3`: a
real `podman volume export`'s own archive imports cleanly via `ociman
volume import`, and vice versa — an `ociman volume export`'s own
archive imports cleanly via a real `podman volume import`.

Integration (`tests/tests/ociman_volume.rs`, 6 new tests):
byte-for-byte round trip through a plain tar; stdin import; gzip
import; import merges onto (rather than wiping) pre-existing content;
export/import of an unknown volume are clear errors.

Regression: all 17 `ociman_volume.rs` tests pass (11 pre-existing + 6
new); full `cargo test --workspace --locked` (111 test result blocks,
0 failures).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).

Performance: `ociman volume export`/`import` are not part of the
hot-path benchmarks tracked in `docs/benchmarks.md` (`ociman run`'s
own container-launch critical path is completely untouched by this
change); no re-benchmark needed.

## Still ahead

`ociman volume mount`/`unmount`/`reload` (real podman's own remaining
volume subcommands, mostly relevant to volume *plugins* this project
has no equivalent of at all — a single fixed "local directory" driver
only) remain out of scope, same as `docs/design/0173` already
established. `ociman images --filter containers=`/`intermediate=`/
`readonly=` (flagged in `0295`) remain separately-scoped candidates.
