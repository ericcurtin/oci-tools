# Design note 0165: `ociman save` (`oci-archive` format)

Status: implemented (`oci-archive` only; `docker-archive` and `ociman
load` deferred — see "What this doesn't do yet")
Scope: `bin/ociman/src/archive.rs` (new module); `bin/ociman/src/
main.rs` (`Command::Save`, `SaveFormat`, `cmd_save`, `write_archive`);
`bin/ociman/Cargo.toml` (new `tar` dependency — already a workspace
dependency elsewhere, see `ci/guards.py`'s own "one crate per
capability" list); `tests/tests/ociman_save.rs`.

## What real `podman save`/`docker save` actually write

Checked directly against `go.podman.io/image/v5`'s own source (not
guessed at): `oci_dest.go` (`oci/archive`) + `oci_dest.go` (`oci/
layout`) for the `oci-archive` format this increment implements,
`tarfile/writer.go`+`types.go`+`dest.go` (`docker/internal/tarfile`)
for the `docker-archive` format real `podman save`/`docker save`
default to (deferred here, see below). Real, hand-verified output from
a real installed `podman save` (arm64, `docker.io/library/busybox`)
confirms the source reading:

```
oci-layout                          {"imageLayoutVersion":"1.0.0"}
index.json                          an OCI image index, one manifest entry,
                                     the tag stashed in an
                                     org.opencontainers.image.ref.name
                                     annotation, no top-level mediaType
blobs/sha256/<hex>                  every blob (manifest, config, each
                                     layer), content-addressed, verbatim,
                                     no re-encoding
```

## Nearly a direct copy of what's already on disk

This project's own `Store` already lays every blob out at exactly
`blobs/sha256/<hex>` (`crates/oci-store/src/lib.rs`'s own documented
layout) — the *same* shape `oci-archive` wants. `save_oci_archive`
therefore does almost no transformation at all: it streams the
manifest, config, and every layer blob straight from `Store::open_blob`
into the tar, unchanged (whatever compression a layer already has,
gzip in every real case this project produces or pulls), and only ever
synthesizes the two small files `oci-archive` needs that aren't
already blobs: `oci-layout` (a fixed, three-field JSON literal) and
`index.json` (built from the real `oci_spec_types::image::ImageIndex`/
`Descriptor` types this project already has, not a bespoke ad hoc
struct — the exact same descriptor shape `oci_registry`/`oci_store`
already use everywhere else, so there is no second, parallel manifest-
descriptor representation anywhere in the codebase).

## `--format` only accepts `oci-archive` — real podman's own default,
`docker-archive`, is a real, separate follow-up

`docker-archive` needs every layer *decompressed* first (the format's
own `dest.go` sets `DesiredLayerCompression: types.Decompress`,
confirmed directly) — a real, nontrivial format conversion this
increment doesn't attempt, rather than a half-right guess at gunzip'd
output. `SaveFormat` (the `clap::ValueEnum` `--format` parses into)
therefore has exactly one variant, `OciArchive`; `--format docker-
archive` is clap's own "invalid value... possible values: oci-
archive" error, not a silent fallback to the wrong thing. `--format
oci-archive` is also the *default* for now (a deliberate, temporary
deviation from real podman's own `docker-archive` default, to be
corrected once `docker-archive` support lands in a follow-up
increment).

## Verified against a real `podman load`, not just self-consistency

Beyond `save_oci_archive`'s own unit test and `ociman_save.rs`'s
integration tests (which check the produced tar's own structure and
every blob's exact byte content directly, self-contained, no external
binary needed), this feature was also verified by hand against real,
installed tools during development:

* A real `podman pull docker.io/library/busybox:latest` followed by a
  real `podman save --format oci-archive` was inspected directly
  (`tar -tvf`/`tar -xf` + reading `index.json`/`oci-layout` by hand)
  to confirm the exact file layout/JSON shape documented above, before
  writing any of this increment's code.
* `ociman pull` then `ociman save --format oci-archive -o out.tar` on
  the same real `busybox` image, followed by a real `podman load -i
  out.tar`, round-tripped correctly: the loaded image kept the exact
  original tag (`docker.io/library/busybox:latest`), the exact
  original config (`arm64`/`linux`, matching `podman inspect`), and
  actually ran (`podman run --rm ... sh -c 'echo hello; uname -a'`
  produced real output).
* A real `docker load` on the same archive (and, separately, on a
  real `podman`-produced `oci-archive` tar of the identical image, to
  rule out this being a bug in this project's own output specifically)
  both failed identically with `invalid archive: does not contain a
  manifest.json` — confirming this is real `docker load`'s own,
  expected behavior (it only understands the `docker-archive`
  format), not a defect in this increment's own output.

## `--output`/stdout, matching real `podman save` exactly

`-o/--output PATH` writes to a real file; with no `--output`, the
archive is written straight to standard output (`ociman save image >
out.tar`, exactly like `podman save image > out.tar`) — and *only* the
archive bytes are ever written to stdout in that case: no digest line,
no `--json` output, matching real `podman save`'s own identical
"nothing else on stdout when the archive itself is going there" shape
(its own progress/status lines go to stderr, matched here by the
existing `oci_cli_common::progress::spinner`, already stderr-only —
see 0155's use of the same helper). When `--output` names a real file
instead, the manifest digest (or, with `--json`, a small
`{reference, digest}` object) is printed to stdout afterward, matching
`ociman push`'s own established convention for the same choice.

## Resolution: tag or image ID, same as `ociman push`

Reuses `resolve_image_by_reference_or_id` verbatim — `ociman save`
accepts a tag reference exactly as pulled/built/tagged, or a real or
short image ID (`ociman images`'s own `DIGEST` column), identical to
`ociman push`/`ociman tag`.

## Tests

`bin/ociman/src/archive.rs`'s own unit test builds a tiny one-layer
image directly in a fresh `Store` (no registry/build machinery
needed), saves it, re-reads the produced tar with the `tar` crate
directly, and asserts every file (`oci-layout`, `index.json`, all
three blobs) is present with the exact expected bytes.
`tests/tests/ociman_save.rs` adds five CLI-level integration tests:
unknown reference is a clear error; `--format docker-archive` is
rejected (naming both the given and the one valid format in its own
error text); a full real archive (seeded via the same
`oci_tools_tests::seed_image` helper `ociman_tag.rs`/`ociman_rmi.rs`
already established) has every expected file with byte-for-byte
correct content and the digest printed to stdout; saving with no
`--output` writes the archive to stdout with nothing else there; and
resolving by a short image ID works. Full `cargo build --workspace
--locked`/`cargo test --workspace --locked` (2 clean runs)/`cargo fmt
--all --check`/`cargo clippy --workspace --all-targets --locked -- -D
warnings`/`python3 ci/guards.py`/`cargo deny check`/`bash
ci/native-ci.sh` all clean.

## What this doesn't do yet

* `docker-archive` format (real podman's own default) — needs a real
  gzip decompression pass per layer plus a synthesized `manifest.json`
  (and, for full legacy compatibility, a `repositories` file and
  per-layer legacy-chain-ID subdirectories — checked directly: real
  `docker load`'s own `ChooseManifestItem` only ever reads
  `manifest.json`, never `repositories` or the subdirectories, so
  those are optional-but-nice-for-older-tooling extras, not
  load-critical).
* `ociman load` (the read side) — entirely deferred; this increment
  only writes archives, verified by loading them with a real, already-
  existing `podman load` rather than this project's own reader.
* `-m`/`--multi-image-archive` (several images in one archive) and
  saving more than one `IMAGE` argument at all — real `podman save`'s
  own multi-image mode, out of scope for this first increment.
* `--compress`/`--uncompressed` (real podman's own flags for
  controlling layer compression when saving to a `dir`-style
  transport) — no `dir`-style transport exists in this project at all
  yet, so neither flag has anything to act on.
