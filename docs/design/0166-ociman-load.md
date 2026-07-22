# Design note 0166: `ociman load` (`oci-archive` format)

Status: implemented (`oci-archive` only, matching 0165's own `ociman
save` scope; `docker-archive` reading remains deferred — see "What
this doesn't do yet")
Scope: `bin/ociman/src/archive.rs` (new `load_oci_archive`,
`LoadedImage`); `bin/ociman/src/main.rs` (`Command::Load`, `cmd_load`,
`LoadResult`); `tests/tests/ociman_load.rs`.

## The read side of 0165

0165 shipped `ociman save --format oci-archive` but deliberately left
`ociman load` itself unimplemented, verifying save's own correctness
by loading its output with a real, already-existing `podman load`
instead. This increment closes that gap with this project's own
reader, matching real `podman load`/`docker load`.

## A single linear pass, no temp-directory extraction

Real `containers/image`'s own `oci-archive` reader extracts the whole
tar to a temporary directory first (`oci/archive/oci_src.go`), since
its underlying `oci/layout` reader needs random access, keyed by
digest, into an arbitrary blob. `load_oci_archive` instead reads the
tar stream exactly once, forward-only:

* Every `blobs/sha256/<hex>` entry is ingested into the store as soon
  as it's encountered, via `Store::ingest_verified` — the exact same
  digest-verifying ingestion a registry pull already uses, so a
  corrupt or hostile archive claiming the wrong content under some
  digest's own filename can never poison local storage (the mismatch
  is caught and rejected the moment that entry is read, not
  discovered later).
* `index.json`/`oci-layout` (both always small) are buffered in memory
  and only interpreted once the whole stream has been drained.

This works directly against standard input (`ociman load < out.tar`,
matching real `podman load`/`docker load`'s own identical usage) with
no temp directory, no extra copy, and no dependency on the input being
seekable at all — a real, measurable simplicity/performance win over
the reference implementation's own approach, since every real archive
this project's own `save_oci_archive` (or a real `podman
save --format oci-archive`) produces always writes every blob before
`index.json`/`oci-layout` anyway (confirmed directly against real
output), so nothing is ever lost by not being able to "look ahead."

## Real, structural validation before trusting anything

After the pass completes: `oci-layout` must be present and parse with
`imageLayoutVersion` exactly `"1.0.0"` (parsed as JSON and read by
field, not a raw byte-for-byte literal compare, since the OCI spec
only actually promises the one field, not a fixed byte layout);
`index.json` must be present, parse as a real `ImageIndex`, and name
**exactly one** manifest (a multi-manifest `index.json` — a real
multi-platform image saved by some other tool — is a clear, named
error, not a silent "picks whichever one" guess, matching this
project's own established "narrow, explicit scope" convention); that
one manifest's own media type must be the single-platform image
manifest type (an image-index/manifest-list entry there is rejected
with the same clarity); the manifest's own blob, its config blob, and
every one of its layer blobs must actually have been ingested from the
archive (checked via `Store::has_blob`, catching an archive whose
`index.json`/manifest names a blob it never actually included).

## Tag handling: mirrors what `save_oci_archive` itself wrote

If the one manifest's descriptor carries the
`org.opencontainers.image.ref.name` annotation (exactly what 0165's
own `save_oci_archive` writes), it's parsed and normalized via the
same `Reference::parse` every other `ociman` command already uses, and
recorded as a real tag pointer (`Store::put_image`) — overwriting
whatever that reference previously pointed at, matching real `podman
load`'s own identical "loading re-tags" behavior. With no such
annotation (an untagged/by-digest-only archive — a real, valid case
this project's own `Store` already supports: blobs with no tag
pointing at them at all), every blob is still ingested and available
by digest, but no tag is recorded; `LoadedImage::reference` is `None`
and `ociman load`'s own success message falls back to the bare digest
(`Loaded image: sha256:...`), matching real `podman load`'s own
identical fallback for the same case.

## Verified against real, independent tools, not just this project's
own round trip

Beyond the automated round-trip test (`ociman save` then `ociman
load`, entirely through this project's own CLI, into a completely
separate store — see below), this was also verified by hand against
real installed tools:

* A real `podman pull` + `podman save --format oci-archive` archive
  (produced entirely independently of this project's own code) was
  loaded with `ociman load` and correctly listed under
  `ociman images` with the right tag and digest.
* The image `ociman load` had just loaded from that real `podman`-
  produced archive was then actually run with `ociman run --rm ...`,
  producing real output (`echo`/`uname -a`) — not just "the metadata
  looks right," a genuinely runnable rootfs made it across.
* This project's own round trip (`ociman pull` -> `ociman save` ->
  `ociman load` into a fresh store) also correctly listed, and ran.

## Tests

`bin/ociman/src/archive.rs` gained four new unit tests:
`save_then_load_round_trips_into_a_fresh_store` (build an image in one
store, save it, load the bytes into a completely separate store,
confirm every blob and the tag match exactly); a no-ref-name-
annotation case (built by re-serializing a real saved archive's own
`index.json` with the annotation stripped, confirming the load still
succeeds but records no tag); a missing-`index.json` rejection; and a
tampered-blob-content rejection (a `blobs/sha256/<hex>` entry whose
real content doesn't hash to what its own filename claims).
`tests/tests/ociman_load.rs` adds four CLI-level integration tests:
a non-archive input file is a clear error; a missing input path is a
clear error; the full save-then-load-then-run round trip through the
real CLI (seeded via the same `oci_tools_tests::seed_image` helper
every other image-focused integration test file already uses); and
`--json` output shape. Full `cargo build --workspace --locked`/`cargo
test --workspace --locked` (2 clean runs)/`cargo fmt --all --check`/
`cargo clippy --workspace --all-targets --locked -- -D warnings`/
`python3 ci/guards.py`/`cargo deny check`/`bash ci/native-ci.sh` all
clean.

## What this doesn't do yet

* Reading `docker-archive` archives — real `podman load`/`docker
  load`'s own more common default-format input, deferred alongside
  0165's own equivalent `docker-archive` *write* gap; the same
  decompression/re-encoding work is needed either direction.
* Multi-manifest (multi-platform) `oci-archive` archives — a real,
  named error rather than an attempt to guess which platform to keep.
* A `dir`-style/unpacked-directory input (real `podman load` can also
  read an already-extracted `oci-dir`) — no `dir`-style transport
  exists in this project at all yet, matching 0165's own identical
  note on the write side.
