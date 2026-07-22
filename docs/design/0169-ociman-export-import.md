# Design note 0169: `ociman export`/`ociman import`

Status: implemented; a real, manually-reproduced bug found and fixed
along the way (see "A real bug found by hand: live mounts and
hardlinks")
Scope: `crates/oci-layer/src/export.rs` (new `export_tree`,
`collect_paths`; `write_entry` gained hardlink deduplication and a
`seen_inodes` parameter, threaded through `export` too);
`crates/oci-layer/src/lib.rs` (export `export_tree`); `bin/ociman/src/
main.rs` (`Command::Export`/`Command::Import`, `cmd_export`,
`cmd_import`, `ImportResult`); `tests/tests/ociman_export.rs`/
`ociman_import.rs`.

## Two new, real podman/docker commands, operating on containers not
images

Unlike 0165-0168's own `save`/`load` (an already-stored *image*'s own
manifest/config/layers, archived and restored as a whole), `export`/
`import` are about a *container's* live filesystem: `ociman export`
writes a container's entire current tree as a real, flat tar (no
layers, no whiteouts, no base-image concept at all — matching real
`docker export`/`podman export` exactly); `ociman import` creates a
brand-new, single-layer image straight from a plain tar (matching real
`docker import`/`podman import`), synthesizing a fresh `ImageConfig`/
`ImageManifest` around it since a plain tar carries none of its own.

## `export_tree`: the whole current tree, not a diff

`oci_layer::export` (already existing, since 0048/0149) is this
crate's own *layer-diff* writer — only what changed relative to an
earlier snapshot. `export_tree` is new: every file/directory/symlink
currently under a root directory, unconditionally, sharing `export`'s
own `write_entry` helper for the actual per-path writing so both stay
byte-for-byte consistent in how a given file/dir/symlink gets
archived.

## A real bug found by hand: live mounts and hardlinks

Manually verifying this feature against a real, still-running
container (not just an already-stopped one, the only case every other
`cp`/`diff`/`commit` test in this project has ever exercised so far)
surfaced two real, separate problems, both found by actually running
the command, not by inspection:

**Live mount-point boundaries.** A running container's `/proc`/`/sys`/
`/dev/pts` are bind-mounted directly onto its own rootfs directory for
its lifetime. A naive recursive walk of that directory (as most-first
drafted) walks straight into those live, effectively-unbounded
pseudo-filesystems too — a real busybox container's export came out at
**490MB** (`/proc`'s own synthetic content) instead of the real ~4MB
image it should have been. Fixed the same way real `tar
--one-file-system`/`rsync -x` both already do it: `export_tree` reads
`root`'s own `st_dev` once, and a subdirectory whose own `st_dev`
differs is still archived as an entry itself (an empty directory —
exactly what a real storage-driver-level export would also show for a
mount point it doesn't otherwise track) but never recursed into.

**Un-deduplicated hardlinks.** Even after fixing the mount-boundary
walk, the same busybox container's export was still far larger than it
should be: real busybox images ship ~380 applets, every one of them a
real hardlink to the exact same ~1.2MB binary — `write_entry` (used by
both `export` and the new `export_tree`) had no hardlink awareness at
all, writing a full, independent copy of that same content once per
applet name (~380 × ~1.2MB ≈ 450MB). Fixed by tracking each real
`(dev, ino)` pair already written once per archive
(`seen_inodes: HashMap<(u64, u64), PathBuf>`, threaded through both
`export` and `export_tree`); a later regular file sharing that pair is
written as a real tar hardlink entry (`tar::Builder::append_link`)
pointing back at the first path, instead of a second full copy —
matching what real `tar`/`docker export` themselves already do for the
identical real content, and directly reusing this crate's own
already-existing `apply`'s `EntryType::Link` handling on the read side
(no new extraction logic needed at all). Confirmed directly: a real
`podman export` of the identical live container produced a **4,410,880-
byte** archive; this project's own fixed `ociman export` produced
**4,408,832** bytes for the same content — a difference of under 2KB,
not the ~490MB it was before either fix.

## `ociman import`: synthesizing a fresh image around a plain tar

The input is normalized through two real scratch files (never held
fully in memory): first decompressed (gzip, detected from the first
two bytes read; anything else assumed already-plain) into a canonical
plain-tar scratch file via `oci_layer::decompress_verifying` (0167),
which also yields the layer's own real `diff_id`; then re-compressed
via `oci_layer::compress_for_storage` (already used by `ociman build`/
`commit`) into this project's own standard gzip encoding for storage.
A real, deliberate two-copy trade-off for simplicity/robustness — this
is a one-shot command, not a hot-path `run`/`rm` benchmark cares about
— matching the same two-tempfile shape `archive.rs`'s own
`append_layer_decompressed`/`ingest_docker_archive_layer` already
established for an analogous conversion.

`--change` reuses the exact same `oci_dockerfile::parse_change` +
`apply_change_instruction` `ociman commit --change` (0164) already
established — the identical 10 Dockerfile-instruction-style overrides,
no duplicated parsing/application logic. `--message` sets the one
synthesized history entry's own `comment`. Real podman's own
`--variant` is deliberately not implemented: this project's own
`ImageConfig` has no `variant` field to set at all yet (a real,
separate, pre-existing gap, not something to grow just for this one
flag).

## Verified against real, independent tools — both directions

* An archive `ociman export` produced (of a real, live, still-running
  container) was imported by a real `podman import` and actually ran.
* A real `podman export`'s own archive (of the identical container
  content) was imported by `ociman import` and actually ran.
* `ociman export | ociman import` (this project's own full round
  trip, through the real CLI end to end) produced a usable, runnable
  image with the exact right content.

## Tests

`crates/oci-layer/src/export.rs` gained real unit tests for
`export_tree` (every current path present with real content; a round
trip through `apply`; an empty directory) and, specifically, a real
hardlink-deduplication test (three hardlinked ~64KB files: archive
size stays well under a naive triple-copy, exactly the real tar
hardlink entry type is used, and re-extracting still produces the
right content under every name). The mount-boundary fix itself is
deliberately **not** unit-tested at the `oci-layer` crate level with a
synthetic mount (that would need `unshare`/`CLONE_NEWNS`, which is
already how this project's own `oci_runtime_core::overlay` probes
rootless overlay support — genuinely real infrastructure this
lower-level, dependency-light crate has no business depending on just
for one test); instead it's covered at the integration level, where a
real, live `ociman run -d` container naturally has real `/proc`/`/sys`
mounts to exercise against —
`export_of_a_still_running_container_completes_quickly_and_excludes_
live_mounts` in `tests/tests/ociman_export.rs` is the real regression
test for both bugs together (a generous but still meaningful bound on
both elapsed time and archive size, since the real point is
distinguishing "fixed" from "walked into /proc" rather than measuring
exact performance). `tests/tests/ociman_import.rs` covers tagging,
untagged imports, reading from standard input, `--change` (including
rejecting a build-only instruction), and the full `export`-then-
`import` round trip through the real CLI. Full `cargo build
--workspace --locked`/`cargo test --workspace --locked` (2 clean
runs)/`cargo fmt --all --check`/`cargo clippy --workspace --all-
targets --locked -- -D warnings`/`python3 ci/guards.py`/`cargo deny
check`/`bash ci/native-ci.sh` all clean.

## What this doesn't do yet

* `podman import`'s own remote-URL `PATH` support (fetching a tarball
  directly from `http(s)://...`) — local file/stdin only for now.
* Any compression beyond gzip on import (`.bzip`/`.xz` real podman
  itself supports) — matches this project's own already-established
  gzip/zstd-only scope elsewhere (`oci_layer::detect_archive`'s own
  doc comment).
* `--variant` (see above).
* `-m`/`--multi-image-archive`-style multi-container `export` — real
  podman itself has no such thing for `export` either (only one
  `CONTAINER` argument, matching this implementation exactly).
