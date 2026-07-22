# Design note 0167: `ociman save --format docker-archive`

Status: implemented (write only; `ociman load` reading `docker-archive`
remains deferred, matching 0165/0166's own oci-archive read/write split)
Scope: `crates/oci-layer/src/compress.rs` (new `decompress_verifying`);
`crates/oci-layer/src/lib.rs` (export it); `bin/ociman/src/archive.rs`
(new `save_docker_archive`, `append_layer_decompressed`,
`DockerArchiveManifestItem`; `append_regular`/`append_blob_from_store`
gained a `mode` parameter, since `oci-archive` and `docker-archive`
use different real fixed values); `bin/ociman/src/main.rs`
(`SaveFormat::DockerArchive`); `tests/tests/ociman_save.rs`.

## Closing 0165's own named deferred gap

0165 shipped `ociman save --format oci-archive` and named
`docker-archive` (real `podman save`/`docker save`'s own *default*
format) as deferred follow-up work, specifically because it needs
every layer decompressed first. This increment does exactly that.

## The real format, checked directly

`go.podman.io/image/v5/docker/internal/tarfile/writer.go` +
`types.go` + `dest.go`, cross-checked against a real installed
`podman save`'s own output (`tar -tvf`): every entry mode `0444`,
mtime the epoch (`1970-01-01`), owned by uid/gid `0` — a real,
different fixed convention from `oci-archive`'s own current-
time/`0644` (not a project inconsistency; the two real formats
genuinely differ here).

```text
manifest.json                 [ {"Config": "<hex>.json", "RepoTags": [...], "Layers": [...]} ]
<config-digest-hex>.json      the image config blob, verbatim
<layer-digest-hex>.tar        each layer, DECOMPRESSED, named by its own real uncompressed digest
```

Deliberately narrower than real `podman save --format docker-
archive`'s own full output: no `repositories` file, no per-layer
legacy-chain-ID subdirectories (`<id>/VERSION`, `<id>/json`,
`<id>/layer.tar`). Checked directly, real `docker load`'s own
`ChooseManifestItem` (`docker/internal/tarfile/reader.go`) never reads
either — only `manifest.json` and the flat files it names — so this is
a real, load-critical-only subset, matching this project's own
established "narrow, explicit, documented scope" convention rather
than a partial/broken implementation.

## The real decompression work: `oci_layer::decompress_verifying`

Added to `oci-layer` (not `ociman` directly) since it's the natural
read-side mirror of that crate's own existing `compress_for_storage`
(same "hash while streaming" shape, same module, same doc-comment
cross-references) — matching this project's own "share as much Rust
code as possible" pillar: any other future caller that ever needs a
layer's real, independently-verified uncompressed digest (not just
`ociman save`) gets it from the same one place.

Deliberately **never trusts the image config's own `rootfs.diff_ids`
blindly** for the layer filename: that value is only ever *asserted*
by whoever built/pushed the image, and this project's own pull path
only verifies the *compressed* blob's digest against the manifest
descriptor (`Store::ingest_verified`), never re-verifies `diff_ids`
against real decompressed content. `decompress_verifying` computes its
own real digest independently, while decompressing, exactly once —
the only value `save_docker_archive` ever uses for the `<hex>.tar`
filename.

Each layer is decompressed into a real scratch file (`tempfile::
NamedTempFile`) — never held fully in memory — so its true size is
known before the tar entry's own header is written and a large layer
never costs a full in-memory copy.

## Tag handling

`RepoTags` is always `[record.reference]` — this project's own
`ImageRecord` always carries exactly one reference (never the
"dangling, zero tags" case real podman/docker's own storage can
produce), so there's no equivalent of real podman's own "no RepoTags
at all" case to handle here.

## `--format` stays defaulted to `oci-archive`, not `docker-archive`,
for now — a deliberate choice, not an oversight

Real `podman save`/`docker save` default to `docker-archive`. Simply
changing `ociman save`'s own default to match would have been the
"more faithful" choice on paper, but `ociman load` doesn't read
`docker-archive` yet (see "What this doesn't do yet") — defaulting
`save` to a format `load` can't consume yet would break this project's
own `ociman save | ociman load` round trip out of the box, a real,
self-inflicted regression this project won't accept just to match a
default value that real interop with actual `podman`/`docker` doesn't
even depend on (a real `podman`/`docker` already defaults to
`docker-archive` regardless of what `ociman`'s own default is — the
default only matters for this project's *own* round trip). Revisit
once `ociman load` also reads `docker-archive`.

## Verified against real, independent tools — both of them

Beyond the automated test (`ociman save --format docker-archive`,
re-reading the produced tar directly and confirming `manifest.json`'s
exact shape, the config file's exact bytes, and every layer file
independently re-decompressed and compared byte-for-byte against the
store's own compressed blob), this was verified by hand against
**both** real tools during development — not just one:

* A real `podman load` of an archive `ociman save --format docker-
  archive` produced: loaded correctly (`docker.io/library/busybox:
  latest`), correct config (`arm64`/`linux`), and the loaded image
  actually ran (`podman run --rm ... sh -c 'echo ...; uname -a'`
  produced real output).
* A real `docker load` of the *same* archive: also loaded correctly
  (as `busybox:latest` — `docker load`'s own convention of dropping
  the default registry's `docker.io/library/` prefix, not something
  this project controls), and the loaded image also actually ran via
  a real `docker run --rm`.

## Tests

`crates/oci-layer/src/compress.rs` gained three new unit tests for
`decompress_verifying`: a round trip recovering the exact same
`diff_id` `compress_for_storage` itself produced plus the exact
original bytes; a no-compression pass-through case; and an explicit
check that the digest returned is always independently computed from
real content, never a caller-supplied assumption.
`tests/tests/ociman_save.rs` gained a `--format docker-archive` test
(the full `manifest.json`/config/layer shape, every layer
independently re-decompressed and compared) plus a renamed/updated
unrecognized-format-value test (now that `docker-archive` is a real,
accepted value, the old "rejects an unimplemented format" test's own
premise no longer held) and a new test confirming the default really
is still `oci-archive`. Full `cargo build --workspace --locked`/`cargo
test --workspace --locked` (2 clean runs)/`cargo fmt --all --check`/
`cargo clippy --workspace --all-targets --locked -- -D warnings`/
`python3 ci/guards.py`/`cargo deny check`/`bash ci/native-ci.sh` all
clean.

## What this doesn't do yet

* `ociman load` reading `docker-archive` — the read-side follow-up,
  deferred alongside this increment (see "why the default stays
  `oci-archive`" above for exactly why this matters).
* A `repositories` file and per-layer legacy-chain-ID subdirectories —
  real, checked-directly non-load-critical extras (see above);
  revisit only if some real tool this project needs to interoperate
  with turns out to actually require them (none observed so far: both
  real `podman load` and real `docker load` loaded this increment's
  own narrower output with no complaint).
* `-m`/`--multi-image-archive` and saving more than one `IMAGE` — same
  deferred scope 0165 already named, unchanged by this increment.
