# Design note 0029: `zstd`-compressed layers

Status: implemented (single-frame streams — see the scope note below)
Scope: `crates/oci-layer` (`Compression::Zstd`), `tests/src/lib.rs`
(`seed_image_with_files_and_compression`).

## The gap

0019 (`oci-layer`) shipped `Compression::Zstd` as an accepted-but-
unimplemented API variant, and `ociman`'s own `compression_for_media_type`
(0020) already correctly routed `application/vnd.oci.image.layer.v1
.tar+zstd` to it — meaning any real image with even one `zstd`-
compressed layer would fail outright with a clear "not supported yet"
error. `zstd` layers aren't a hypothetical: they're an increasingly
common real choice (better compression ratio and decompression speed
than gzip), used by real, current image builds — not implementing this
meant a real, growing class of images this project aims to be a
drop-in replacement for simply couldn't be pulled and run at all.

## Pure Rust, matching this project's own gzip precedent

`ruzstd` (MIT, `~/git`'s own capability-group precedent for gzip —
`flate2`'s Rust backend, avoiding a C zlib dependency — applied the
same way here): a mature, actively maintained, pure-Rust Zstandard
decoder (and encoder, used only by this increment's own tests, not
shipped code) with no `libzstd` FFI dependency at all, keeping this
project's all-Rust design intact. Added a `"zstd decompression"` entry
to `ci/guards.py`'s capability-group table (alongside `zstd`/
`zstd-safe`, the FFI-wrapping alternatives it guards against) so a
second, competing zstd crate can't sneak in later, matching the
existing `"gzip decompression"` entry's own precedent exactly.

`ruzstd::decoding::StreamingDecoder` implements `std::io::Read` (once
its own eager, fallible construction — it validates the zstd frame
header immediately, unlike `flate2`'s lazy-on-first-read `GzDecoder` —
succeeds), so it drops into `oci_layer::apply`'s existing `impl Read`-
based pipeline with no structural change: `apply_tar(decoder, dest)`,
exactly like the `Gzip` branch already does.

## Scope limit: single-frame streams only

`ruzstd`'s own documentation is explicit about this: `StreamingDecoder`
expects its input to be a *single* zstd frame, while the format itself
permits concatenating several in one archive. Handling multiple frames
would need the caller to catch a specific `SkipFrame` error and re-
create the decoder in a loop (per `ruzstd`'s own upstream issue #57) —
not implemented here. Every real `zstd`-compressed OCI layer blob this
project has ever pulled or synthesized (including this increment's own
test fixture, built with `ruzstd`'s own encoder) is a single frame —
the shape every common encoder produces by default — so this is a
real, documented, but not yet observed-to-matter scope limit, the same
kind of principled compromise this project has made repeatedly
elsewhere (seccomp's single-shared-action scope, 0016; the single-
mapped-uid limitation, 0013/0024).

## Real, automated tests

`crates/oci-layer/src/lib.rs`'s own unit tests (2 new cases): a real
`ruzstd`-compressed tar archive round-trips through `apply` identically
to the existing uncompressed/gzip cases; a deliberately malformed
"zstd" stream (plain garbage bytes) produces a clear
`LayerError::InvalidZstd`, not a panic or a misleading message.

`tests/tests/ociman_run.rs` gained `run_extracts_a_zstd_compressed_layer`
— a real end-to-end `ociman run` against a synthetic image whose one
layer is genuinely `zstd`-compressed (not just `oci-layer`'s own unit
test in isolation), proving the whole pull-manifest-media-type ->
`compression_for_media_type` -> `oci_layer::apply` -> launch pipeline
actually works together for this case, the same "prove it end to end,
not just at the unit level" standard 0020's own real-image-first
testing approach established.

`tests/src/lib.rs`'s `seed_image_with_files` is now a thin wrapper
around a new `seed_image_with_files_and_compression` (taking an
explicit `LayerCompression::{Gzip,Zstd}`), so the new zstd test didn't
need to duplicate any of the existing tar-building logic, and none of
`seed_image_with_files`'s three existing call sites needed to change.

## Performance

Doesn't touch `oci_runtime_core`/`launch` at all — this is purely a new
decompression code path inside `oci-layer`, exercised once per `zstd`-
compressed layer during image extraction, nowhere near the `run`
fork-to-exec hot path this project has been benchmarking. No
re-benchmark needed, consistent with prior increments that only
touched non-hot-path code. `ociman`'s own binary grew by about 180KB
(`ruzstd` plus its `twox-hash` dependency) — a modest, reasonable cost
for gaining real support for an increasingly common real-world layer
format; `ocirun` (which doesn't depend on `oci-layer`) is completely
unaffected.

## What's still not here

* Multi-frame zstd streams (see the scope-limit section above).
* Layers compressed with anything other than gzip/zstd/uncompressed
  (the full OCI media-type list has no other compression variants in
  practice, so this isn't currently expected to matter).
