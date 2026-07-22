# Design note 0168: `ociman load` reads `docker-archive`

Status: implemented; `save`'s own default format changed back to
`docker-archive` (see below); a real interop bug found and fixed along
the way (see "A real bug, found by hand")
Scope: `bin/ociman/src/archive.rs` (`load_archive` replaces
`load_oci_archive`, now format-auto-detecting; new
`load_docker_archive_manifest`/`load_oci_archive_index`/
`ingest_docker_archive_layer` helpers; `LoadedImage.reference` ->
`references: Vec<String>`; `DockerArchiveManifestItem` gained
`Deserialize`; new `ANNOTATION_CONTAINERD_IMAGE_NAME` constant);
`bin/ociman/src/main.rs` (`SaveFormat::DockerArchive` is the default
again; `cmd_load`/`LoadResult` updated for multiple references);
`bin/ociman/Cargo.toml` (`flate2` dev-dependency, for a test);
`tests/tests/ociman_save.rs`/`ociman_load.rs`.

## Closing 0167's own named deferred gap

0167 shipped `ociman save --format docker-archive` and named the read
side ("`ociman load` reading `docker-archive`") as the one deferred
piece, specifically because `save`'s own default stayed `oci-archive`
to avoid breaking this project's own round trip. This increment closes
that gap — and, because it does, also revisits that default (see
below).

## Auto-detection, matching real `podman load`/`docker load` exactly

Neither real tool takes a `--format` flag on load — both auto-detect
from the archive's own contents. `load_archive` does the same: a
single linear pass accumulates whichever format-defining files it
actually finds (`index.json`/`oci-layout` for `oci-archive`;
`manifest.json` for `docker-archive`), ingesting every blob-shaped
entry as it's encountered regardless of which format it'll turn out to
be, then decides which format it was and finishes the load only once
the whole stream has been consumed. Neither file present is a clear,
named error; both a real `oci-archive` (has `index.json`) and a real
`docker-archive` (has `manifest.json`, no `index.json`) are handled by
the exact same one function.

## `docker-archive` reading has to *synthesize* a manifest that never
existed on disk

Unlike `oci-archive` (which stores a real OCI manifest blob),
`docker-archive` never stores one at all — only `manifest.json`'s own
flatter `Config`/`RepoTags`/`Layers` description. Reading it back
therefore means:

* Each top-level `<hex>.tar` (a plain, uncompressed layer — the
  format's own convention) is gzip-compressed while streaming straight
  into the store via `oci_layer::compress_for_storage` — the exact
  same helper `ociman build`/`commit` already use — which also yields
  that layer's own real, independently-computed uncompressed digest
  (the `diff_id`).
* The config blob (a top-level `<hex>.json`) is ingested verbatim — no
  transformation, since the config schema is shared between OCI and
  Docker (established in 0165/0167).
* The config's own `rootfs.diff_ids` is cross-checked against every
  layer's own freshly, independently computed `diff_id` — **never**
  assumed to already match just because the archive claims so. A
  mismatch is a real, clear, refused load, not a manifest that would
  misdescribe what's actually in the archive.
* A fresh, real OCI `ImageManifest` (schema version 2, real OCI media
  types throughout) is built wrapping the unchanged config and the
  freshly re-compressed layers, and ingested as this image's own new
  manifest blob — this project's own canonical shape from here on,
  regardless of which format it was loaded from.

## Tagging: every `RepoTags` entry, not just the first

Real `docker load` tags a loaded image under *every* `RepoTags` entry
`manifest.json` names, not just one. `LoadedImage.reference` (a single
`Option<String>`) became `references: Vec<String>` to represent this
correctly — `oci-archive` still only ever produces at most one (since
`save_oci_archive` only ever writes one `ref.name` annotation), but
`docker-archive` (from some *other* tool, or a future `ociman save`
multi-tag mode) can produce several. `ociman load`'s own output prints
one `Loaded image: ...` line per reference (or the bare digest if
none), matching the plural case naturally.

## A real bug, found by hand: `io.containerd.image.name` vs.
`org.opencontainers.image.ref.name`

While manually verifying this increment against a real, installed,
modern `docker save` (Docker 29.2.1, `buildkit`-based), the produced
archive turned out to be a real hybrid: **both** `index.json`/
`oci-layout` (a genuine `oci-archive` half) *and* `manifest.json`/
`repositories` (a legacy `docker-archive` half), sharing one
`blobs/sha256/` directory. Loading it (via the `oci-archive` half,
since `index.json` was present) produced the wrong reference:
`docker.io/library/latest:latest` instead of `docker.io/library/
busybox:latest`.

Root cause, found by inspecting the real archive's own `index.json`
directly: modern `docker save` sets `org.opencontainers.image.ref.name`
to just the bare tag (`"latest"`) — which is, per the OCI image-spec's
own prose, technically what that annotation is documented to mean (“the
name of the reference”), just not what real podman's own writer
(`save_oci_archive`'s own model) puts there (the *full* reference). The
archive's real full reference lives under a second, non-spec
annotation instead: `io.containerd.image.name`.

Checked directly against real podman's own source
(`~/git/container-libs/common/libimage/pull.go`'s own
`nameFromAnnotations`, which explicitly cites the real upstream issue
this exact mismatch caused, `containers/podman/issues/12560`): real
podman prefers `io.containerd.image.name` first, falling back to
`org.opencontainers.image.ref.name` only if absent. `load_archive` now
does the exact same thing — confirmed fixed by re-running the same
real archive through `ociman load` afterward (correct reference,
correct `ociman run`).

## `save`'s own default changes back to `docker-archive`

0167 kept `--format`'s default at `oci-archive` specifically because
`ociman load` couldn't read `docker-archive` yet, and defaulting
`save` to a format `load` couldn't consume would have broken this
project's own round trip. That's no longer true — `load` reads both
now — so the default reverts to `docker-archive`, matching real
`podman save`/`docker save`'s own default exactly, removing the one
remaining documented deviation from 0167.

## Verified against real, independent tools — three separate real
archives, not just this project's own output

* This project's own default round trip (`ociman save` with no
  `--format`, now `docker-archive` by default, then `ociman load` into
  a fresh store) still works end to end, including a real `ociman run`
  of the result.
* A real `podman save`'s own (legacy-shaped) `docker-archive` output
  loaded correctly and ran.
* A real, modern `docker save`'s own (hybrid oci-archive/docker-archive)
  output loaded correctly (after the `io.containerd.image.name` fix
  above) and ran.

## Tests

`bin/ociman/src/archive.rs` gained: a docker-archive round-trip test
(manifest synthesis, `diff_id` cross-check, multiple `RepoTags` all
tagged); a `diff_id`-mismatch rejection test; an `oci-layout`-missing
rejection test (now a real, distinct case from the "neither format"
one); a regression test reproducing the exact real
`io.containerd.image.name`-vs-`ref.name` bug above. `tests/tests/
ociman_save.rs`'s format-default test now asserts `docker-archive`;
`tests/tests/ociman_load.rs`'s JSON-output test now checks a
`references` array. Full `cargo build --workspace --locked`/`cargo
test --workspace --locked` (2 clean runs)/`cargo fmt --all --check`/
`cargo clippy --workspace --all-targets --locked -- -D warnings`/
`python3 ci/guards.py`/`cargo deny check`/`bash ci/native-ci.sh` all
clean.

## What this doesn't do yet

* Multi-image `docker-archive` archives (`manifest.json` naming more
  than one image) — a real, named error, matching `oci-archive`'s own
  identical single-manifest-only scope.
* A `repositories` file/legacy per-layer subdirectories on the *write*
  side — unchanged from 0167 (still not written; still not needed for
  a real `docker load`/`podman load` to succeed).
* `-m`/`--multi-image-archive` — unchanged deferred scope from 0165.
