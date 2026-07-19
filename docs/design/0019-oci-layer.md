# Design note 0019: `oci-layer` (tar/whiteout applier)

Status: implemented (new crate; not yet wired into `ociman`)
Scope: `crates/oci-layer` — `apply(reader, compression, dest)`.

## The gap this closes

The README's own design pillars have named this component since
milestone 1 ("one tar/whiteout applier"), and it's the one piece
`ociman run` needs that nothing in the workspace provided yet: turning
a pulled image's layer blobs (already in `oci-store`'s content-
addressed store, exactly as downloaded — still compressed tarballs,
per `oci-store`'s own scope) into an actual root filesystem directory
`oci-runtime-core::launch` can run a container against. This increment
is the extraction primitive alone, verified thoroughly on its own
before the next increment wires it into `ociman run` (pull-if-missing,
layer-by-layer application in order, image-config -> runtime-spec
synthesis) — matching this project's established pattern of landing
one well-verified piece at a time rather than one large, harder-to-
review feature.

## Whiteouts, ported from `moby`'s own reference implementation

The OCI image-spec's whiteout convention (`.wh.<name>` deletes
`<name>`; `.wh..wh..opq` marks a directory opaque, hiding everything
beneath it that came from a lower layer) originates from `moby`/Docker.
Read `moby`'s own implementation
(`vendor/github.com/moby/go-archive/diff.go`'s `UnpackLayer`) rather
than re-deriving the exact semantics from the spec prose alone, because
one detail is easy to get wrong: the opaque-whiteout marker can appear
*before or after* the layer's own real entries for that directory in
the tar stream, and only entries from *lower* layers should be removed
— never something the very same layer just wrote earlier in the same
pass. `moby` tracks an explicit "already unpacked this call" set for
exactly this reason; `oci_layer::apply_tar` does the same
(`written: HashSet<PathBuf>`), and two tests
(`opaque_whiteout_removes_lower_layer_siblings_but_keeps_this_layers_
own_entries` / `..._after_the_new_entries_still_keeps_them`) check both
orderings explicitly rather than only the common one.

Legacy AUFS-only artifacts (`.wh..wh.plnk`, a hardlink-redirect
directory some exporters still emit for the long-obsolete AUFS graph
driver) are deliberately not handled — this project targets overlay-
style filesystems only, never AUFS, and no current graph driver
ecosystem needs that hack either.

## Real-world verification: a real image, not just synthetic tar fixtures

Unit tests (14, all synthetic — files, directories, symlinks, hard
links, both whiteout kinds in both stream orderings, path-traversal/
absolute-path rejection via hand-crafted headers the `tar` crate's own
safe `Builder` API refuses to construct, gzip round-tripping) cover the
logic in isolation. Real-world verification went further: pulled a
real `busybox` image (`podman pull`/`podman save`, then deleted after),
extracted its one real layer (448 real tar entries, the large majority
hard links — busybox's entire `/bin` is ~380 links to one multi-call
binary) through both a scratch program using this crate and the
system's own `tar` binary, and `diff -rq`'d the two resulting
directory trees: **zero differences**, inode-sharing for the hard
links confirmed via `ls -i`. Repeated with the same real layer
re-compressed with `gzip` through `Compression::Gzip`, same result.
This is the first time in the project real, unmodified upstream image
data (not a hand-built fixture) has been used to verify a new crate's
correctness directly, rather than a spec-derived fixture or a runc/
crun-generated config.

## Honest, narrowly-scoped gaps (this crate's own doc comment covers all of these)

* **`zstd` compression** — accepted by the `Compression` enum, returns
  a clear `ZstdNotSupported` error rather than silently mishandling it.
  `gzip` is by far the dominant real-world media type; `zstd` support
  is deferred rather than pulling in a C `zstd` library (the `zstd`
  crate vendors and compiles real C zstd source) purely to cover a
  minority case — a pure-Rust decoder (`ruzstd`) is a candidate for a
  later increment if it proves mature enough.
* **Ownership** — extracted files keep the tar entry's permission bits
  but are never `chown`ed to the entry's `uid`/`gid` (no privilege to
  do so rootlessly, and no subordinate-uid-range remap set up yet).
  Correct for the overwhelmingly common case (root-owned image files,
  which is exactly what a rootless container's own id-mapping already
  resolves the calling user to *inside* the container), wrong for a
  file intentionally owned by some other uid in the image.
* **Device nodes and FIFOs** — skipped, not attempted: creating a real
  device node needs `CAP_MKNOD`, which a rootless caller never has on
  the host. The same wall real `podman`/`buildah` hit.
* **Extended attributes** (SELinux labels, `security.capability`,
  ...) — not preserved.

## What's still not here

* Wiring into `ociman run`: pull-if-missing, applying every layer in
  a manifest in order (bottom to top) into one bundle's `rootfs/`, and
  synthesizing a runtime-spec `config.json` from the image's
  `ContainerConfig` (`Cmd`/`Entrypoint`/`Env`/`WorkingDir`/`User`) —
  the actual `ociman run` command, planned as the next increment on
  top of this one.
