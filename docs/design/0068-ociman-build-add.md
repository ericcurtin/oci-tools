# Design note 0068: `ociman build` `ADD` (milestone 4)

Status: implemented (local sources only — archive-auto-extraction and
plain-file fallback; remote URL sources are a separate, later
increment — see "What's still not here")
Scope: `crates/oci-layer/src/detect.rs` (new), `crates/oci-layer/
src/lib.rs`, `bin/ociman/src/build.rs` (new `add_instruction`,
`needs_rootfs` gate now includes `Instruction::Add`), `tests/tests/
ociman_build.rs`.

`ADD` has been parsed since early in `oci-dockerfile`'s own history
(`Instruction::Add`/`AddFlags`) but `ociman build` itself has always
rejected it outright with a clear "not yet supported" error — one of
milestone 4's own longest-standing, explicitly-named gaps.

## The one real behavioral difference from `COPY`, checked directly against the currently-vendored real source

Real docker's own documented `ADD` behavior beyond `COPY`: a local,
non-directory source that's a recognized archive is unpacked into the
destination instead of copied as one file; a remote URL source is
downloaded first. Checked directly, not from memory or older
documentation: `~/git/moby/daemon/builder/dockerfile/copy.go`'s own
`performCopy` (`options.decompress && archive.IsArchivePath(srcPath)`,
gated on `src.IsDir()` being false first) and the vendored
`archive.IsArchivePath`/`compression.DetectCompression`
(`~/git/moby/vendor/github.com/moby/go-archive/archive.go`,
`.../containerd/containerd/v2/pkg/archive/compression/compression.go`).
One real, worth-noting finding from reading the actual vendored code
rather than assuming: *this exact vendored version* of
`DetectCompression` only recognizes gzip and zstd — not bzip2/xz,
which some older real `docker` releases did support. Matching the
version genuinely vendored in this workspace's own `~/git/moby`
checkout (not a hypothetical maximal feature set) is the honest,
checked choice.

## `oci_layer::detect_archive` — one implementation, not a second tar/gzip/zstd stack in `bin/ociman`

`ci/guards.py`'s own "one crate per capability" rule already reserves
`tar`/`flate2`/`ruzstd` for `oci-layer` alone. Real docker's own
`IsArchivePath` doesn't stop at magic-byte detection — it actually
tries to decompress and parse a real tar header, specifically to avoid
the false positive of "a gzip-compressed file whose content merely
isn't a tar archive at all". `detect_archive` matches that exact
two-step check (a magic-number check first, cheap and matching real
docker's own order, then a real decompress-and-parse-one-tar-header
attempt) and lives in `oci-layer` precisely so `bin/ociman` never needs
its own second copy of gzip/zstd/tar handling just for this one check
— it reuses the exact same `Compression`/`apply` this crate already
uses for real OCI layers.

## Manual, end-to-end verification before writing a single automated test

Built the debug binary and ran four real scenarios by hand first: a
real `gzip`-compressed tar (two files, one nested in a subdirectory)
`ADD`ed into a fresh destination — extracted correctly, subdirectory
structure intact, destination directory created automatically; a plain
non-archive file `ADD`ed — copied verbatim exactly like `COPY`; a real
gzip stream whose content is deliberately *not* a tar archive — copied
verbatim as the still-gzipped file, *not* mistakenly unpacked (the
exact false-positive case `IsArchivePath`'s own two-step check exists
to avoid); a real directory source — copied recursively, its own
*contents* landing under the destination, matching real docker's rule
that directory sources are never auto-decompressed at all (checked
directly: `performCopy`'s own `src.IsDir()` branch returns early,
before the archive check is ever reached). A remote URL source was
confirmed to fail with a clear, real error rather than being
misinterpreted as a local path.

## Real, automated tests

6 new tests in `oci-layer::detect` (a plain uncompressed tar; a real
gzip-compressed tar; a plain text file is not an archive; a real gzip
stream whose content isn't a tar is not an archive — the exact false
positive above; an entirely empty tar archive is not considered an
archive either, matching real docker's own `io.EOF`-on-`Next()`
behavior for that same edge case; data that merely *starts with* the
gzip magic bytes but isn't real gzip at all is not an archive). 3 new
integration tests in `tests/tests/ociman_build.rs`, each running a real
build and then the resulting image: a local gzip tar archive is
extracted with its nested structure intact; a non-archive source
(including the gzip-but-not-tar false-positive case) is copied like
`COPY`; a remote URL source is rejected with a clear error and no
partial image left tagged.

## Performance

This increment touches only the still growing-in-scope
`oci-layer::detect` (a brand new module, calling `oci-layer`'s own
existing `Compression` type but none of its existing functions) and
`bin/ociman/src/build.rs`'s own `ADD`-handling — `oci-runtime-core`,
`main.rs`'s `synthesize_spec`/`resources_from_cli`, and either cgroup
driver are completely untouched (confirmed via `git diff --stat`), and
neither `ocirun` nor `oci-runtime-core` depends on `oci-layer` at all,
so there is no plausible mechanism for this change to affect
container-startup/destroy performance — no benchmark re-verification
was needed, consistent with how this project has always treated
build-only increments (`--build-arg`, the unused-build-arg warning)
that don't touch the run/startup hot path.

## What's still not here

* Remote URL sources (`ADD http://...`) — needs a general-purpose HTTP
  client this project doesn't have a sanctioned one for yet outside
  `oci-registry`'s own registry-protocol-specific one; a genuinely
  separate, later increment.
* Everything `COPY` itself still doesn't support yet (multiple
  sources, glob patterns, `--chown`/`--chmod`) — `ADD` shares exactly
  the same scope limits, by design, rather than exceeding `COPY`'s own
  feature set in unrelated ways.
* `COPY --from=<external-image>`, the build cache, `ONBUILD`/
  `HEALTHCHECK`, an anonymous/untagged build mode, `--target` — all
  still exactly as before.
