# Design note 0209: `oci_layer::export`/`export_tree` forced-mtime support

Status: implemented (shared primitive only; `ociman build --timestamp`
itself is a separate, still-ahead follow-up)
Scope: `crates/oci-layer/src/export.rs` (`export`, `export_tree`,
`write_entry`, `write_whiteout`); `crates/oci-dockerfile/src/commit.rs`
(`commit_layer`, `squash_layer`); every call site of all four
(`crates/oci-layer/src/compress.rs`'s own test, `bin/ociman/src/
build.rs`, `bin/ociman/src/main.rs`).

## Closing half of 0199's own deferred gap

0199 (`ociman build --build-arg-file`) investigated `--timestamp`
first and deliberately deferred it whole: "real `podman build
--timestamp` doesn't just rewrite `created`/history metadata, it also
normalizes newly-committed layers' own file mtimes, which would need
`oci_layer::export` itself (shared by several other commands) to
support a forced mtime too." This increment builds exactly that
missing primitive, verified thoroughly on its own, before attempting
the full CLI integration in a future increment — the same "narrow
first slice" pattern this project has used repeatedly (e.g.
`oci_store::cache_root` before `ociboot build-image`, `oci_registry::
resolve_or_pull` before `ocibox create`).

## Confirmed against real buildah/podman source first

Traced the real flag all the way down before writing any code:
`~/git/podman/vendor/go.podman.io/buildah/pkg/cli/common.go`'s own
`--timestamp` flag definition ("set new timestamps in image info and
layer to seconds after the epoch, defaults to current times") →
`buildah/commit.go`'s own `destinationTimestamp` (set from
`HistoryTimestamp`, unconditionally when `--timestamp` was explicitly
given, matching `Option<i64>`'s own `None`-means-"not given" semantics
exactly — real buildah checks `Flag("timestamp").Changed`, not just
whether the value is nonzero) → `buildah/common.go`'s
`getCopyOptions` → `image/v5/copy.Options.DestinationTimestamp` →
`private.CommitOptions.Timestamp` → and finally `containers/storage`'s
own `pkg/archive/archive.go`: `TarOptions.Timestamp`, if set,
overwrites `ModTime`/`AccessTime`/`ChangeTime` on **every** written tar
header. This confirms the real, checked-directly behavior this
increment ports: a forced timestamp overrides every entry's own mtime,
not just the image's own metadata fields.

## What changed

`oci_layer::export`/`export_tree` both gained a new `forced_mtime:
Option<i64>` parameter (seconds since the epoch). `write_entry`
(regular files, directories, symlinks, and the hardlink-dedup fast
path) and `write_whiteout` each override the header's own mtime with
this value when given, leaving every other field (mode, uid, gid,
content, entry type, link target) exactly as before.

`write_entry`'s own non-hardlink path used to call the convenient
`tar::Builder::append_path_with_name` directly, which builds its own
header internally with no way to intercept it before writing. Replaced
with the exact same lower-level calls that convenience method uses
internally (`Header::set_metadata` — the identical `HeaderMode::
Complete` fill real `podman`'s own analogous "complete metadata" path
uses too — then `Builder::append_data`/`append_link`), verified to
produce byte-identical output to before whenever `forced_mtime` is
`None` (every one of the 60+ pre-existing `oci-layer` tests, several of
which check exact header fields/entry types/link targets, pass
completely unmodified).

`oci_dockerfile::commit_layer`/`squash_layer` (the two real callers
that turn a diff/whole-tree into a stored layer) each gained the same
`forced_mtime: Option<i64>` parameter, threaded straight through to
their own `oci_layer::export`/`export_tree` call. Every existing call
site across `ociman build`'s `RUN`/`COPY`/`ADD`/`--squash`/
`--squash-all` and `ociman commit`/`ociman commit --squash` now passes
`None` explicitly — real, live wall-clock mtimes, completely unchanged
behavior — since none of them has a `--timestamp` flag of their own
yet.

## Verified by hand

Two builds of byte-identical content, each with a real file
deliberately given a different real mtime (one day apart, one month
apart), still produce the exact same content-addressed layer digest
when the same `forced_mtime` is given to both — the concrete
reproducibility guarantee this primitive exists to enable, checked at
both the `oci_layer::export_tree` level (raw bytes) and the
`oci_dockerfile::commit_layer`/`squash_layer` level (stored blob
digest). With no `forced_mtime` (`None`), a file's own real mtime
reaches the archive completely unchanged, confirmed directly.

Ran a full `ociman build` (a real Containerfile with `RUN`/`COPY`) and
`ociman build --squash` against a real pulled `busybox` image end to
end afterward — both produced working images whose containers ran
correctly, confirming every touched call site (`RUN`, `COPY`,
`--squash`) still functions with `forced_mtime: None`, not just the
unit tests.

## Tests

Nine new unit tests in `crates/oci-layer/src/export.rs` (forced-mtime
override for a regular file, directory, symlink, hardlink entry, and
whiteout entry; a byte-identical-despite-different-real-mtimes
comparison; and an explicit "no override leaves the real mtime alone"
check) plus two new unit tests in `crates/oci-dockerfile/src/
commit.rs` (`commit_layer`/`squash_layer` each produce identical
digests for otherwise-identical content committed at different real
times, given the same `forced_mtime`). Every pre-existing test in both
crates continues to pass completely unmodified.

Full `cargo build --workspace --locked`/`cargo test --workspace
--locked` (2 clean runs, 88/88 result blocks — no new test binaries,
just 11 more tests split across two existing `oci-layer`/
`oci-dockerfile` unit-test binaries: 63/152 respectively, up from
54/150)/`cargo fmt --all --check`/`cargo clippy --workspace
--all-targets --locked -- -D warnings`/`python3 ci/guards.py`/`cargo
deny check`/`bash ci/native-ci.sh` all clean. No performance regression
(`ociman run --rm`, ~65ms, consistent with prior measurements — this
change only affects layer-committing code paths, not container
startup at all).

## What this doesn't do yet

`ociman build --timestamp` itself (the CLI flag, threading a parsed
`--timestamp <seconds>` value down into every `commit_layer`/
`squash_layer` call site in `build.rs`, and overriding every new
`HistoryEntry.created`/the image's own top-level `created` field to
match — the "image info" half of the real flag's own doc string,
distinct from this increment's "layer" half) is a separate, still-
ahead follow-up, deliberately scoped out of this increment to keep it
small and independently verifiable. `ociman commit --timestamp` (real
buildah's own `commit` supports it too) is unscoped entirely for now.
