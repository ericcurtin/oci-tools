# Design note 0061: `oci-erofs` — real `mkfs.erofs` builder (milestone 5, first increment)

Status: implemented (build-only; sealing/verification/ociboot glue are
separate, larger follow-ups — see "What's still not here")
Scope: `crates/oci-erofs/src/builder.rs` (new), `crates/oci-erofs/
src/lib.rs`, `crates/oci-erofs/Cargo.toml`, `ci/vm-prepare.sh`.

Milestones 1-4 are all container/runtime work (`ocirun`/`ociman`); this
is the first increment into milestone 5 (`ociboot`, the bootc
equivalent), which until now was pure skeleton — `oci-erofs`/`oci-bls`
were both 15-17 line stub crates with nothing but a planning doc
comment, and `bin/ociboot`/`bin/ociboot-init` both unconditionally
`bail!`/exit(2) with "milestone 1 skeleton, nothing implemented yet".
That plan already named the right first piece: *"backend trait with an
`mkfs.erofs` driver implementation first ... one of the few sanctioned
external-tool escape hatches, wrapped behind a trait"* — this
increment is exactly that, and nothing more.

## Why `mkfs.erofs` at all, and why a trait around it

`docs/HACKING.md` names `mkfs.erofs` explicitly as one of the small set
of sanctioned shell-outs (alongside `mkfs.ext4`/`mkfs.xfs`,
`veritysetup`, `grub2-mkconfig`/`grub-install`, `dracut`,
`sfdisk`/`blkid`) — erofs image *writing* is a niche, format-specific
operation with no mature pure-Rust crate available, unlike gzip/zstd/
tar (all wrapped in pure Rust elsewhere in this workspace already).
The crate's own pre-existing doc comment already called for a trait
so a future feature-gated pure-Rust writer could implement the same
interface later without touching any caller — `ErofsBuilder` (this
increment) plus `MkfsErofs` (its first, real implementation) is that
shape, matching `oci-mount`'s own `syscalls`/`options` split: thin,
mechanical, no policy of its own.

## Determinism verified directly against the real binary, not assumed

The entire reason `ociboot` wants erofs images at all (see `docs/
HACKING.md` and `bin/ociboot/src/main.rs`'s own module doc) is
reproducibility: "same manifest digest in, bit-identical image out".
Before writing any Rust, this was checked by hand against the real
`mkfs.erofs 1.7.1` binary installed on this host: a small source tree
(two regular files, a subdirectory, a symlink) built twice more than a
second apart in wall-clock time, with `-T0` (fixed timestamp),
`-U<fixed-uuid>`, and `--all-root` all given explicitly, produced two
images whose sha256 matched exactly, byte for byte (`cmp` reported no
difference). The resulting image was also loop-mounted directly
(`mount -t erofs -o ro`) to confirm it's a real, correctly-populated
filesystem — file contents, the symlink target, and the subdirectory
were all exactly as expected, with every entry's owner normalized to
root by `--all-root` and every mtime pinned to the Unix epoch by `-T0`
regardless of the source tree's own real timestamps.

This is why [`BuildOptions`] has no `Default` impl and no field is
optional in a way that could silently mean "now"/"random": `timestamp`
and `uuid` are always caller-supplied. A silent "current wall-clock
time" default would make it trivially easy to build a non-reproducible
image by accident and not notice until two supposedly-identical builds
produced two different digests down the line.

## What this crate does *not* decide

`oci-erofs` never derives `timestamp`/`uuid` from a manifest digest
itself — that's `ociboot`'s own policy to own once it exists (a later,
larger milestone-5 increment), matching this workspace's established
split between "mechanism" crates (`oci-runtime-core`'s cgroup/seccomp/
mount primitives) and the CLI binaries that supply policy into them
(`ociman`'s `synthesize_spec`/`resources_from_cli`). Keeping that
derivation here would make this crate stop being a thin, honest
wrapper for no real benefit.

## Error handling

`mkfs.erofs`'s own diagnostics are a `<E> erofs: ...` line followed by
a full usage dump, all on stderr (checked directly: `--quiet` only
suppresses the success-path progress output, not error reporting, and
even suppresses that only on stdout — the version banner still prints
there regardless). `MkfsErofs::build` captures stderr via
`Command::output` and surfaces just the first line in its returned
`io::Error`, giving a caller a clean, specific message (e.g. `mkfs.erofs
exited with exit status: 1: <E> erofs: invalid volume label`) instead
of a multi-page usage dump.

## Real, automated tests

5 tests in `oci-erofs::builder`, all against the real `mkfs.erofs`
binary (each skips itself with a clear message rather than failing if
`mkfs.erofs` genuinely isn't installed, matching this workspace's
existing pattern of environment-gated privileged tests):
building a real image and checking its on-disk superblock magic number
(`0xE0F5E1E2` at the fixed 1024-byte `EROFS_SUPER_OFFSET`, checked
directly against `erofs-utils`' own `erofs_fs.h`); the same source tree
built twice, more than a second apart in wall-clock time, producing a
bit-identical image; `--all-root` succeeding regardless of a source
file's own real permission bits; a missing source directory surfacing
as a real, useful `io::Error`; an overlong volume label being rejected
by the real binary with its own message passed through.

## CI: `erofs-utils` added to both VM base images

`ci/vm-prepare.sh` installed `e2fsprogs` already (for `mkfs.ext4`, used
by `ci/vm-ci.sh`'s own cache-device setup) but nothing providing
`mkfs.erofs`. Added `erofs-utils` to both the `dnf` (CentOS Stream 10)
and `apt-get` (Ubuntu 26.04) package lists — the same package name on
both distros — so the new tests run for real inside CI, not just
locally, exactly like every other privileged/tool-dependent test in
this workspace already does.

## Performance

This increment touches only the brand-new `oci-erofs` crate and
`ci/vm-prepare.sh` — `oci-runtime-core`/`ocirun`/`ociman run`'s own hot
paths are completely untouched (confirmed via `git diff --stat`), so
no benchmark re-verification was needed. `oci-erofs` isn't linked into
any binary's own hot path yet either (nothing calls it outside its own
tests) — the crate compiles and is exercised, but has zero runtime
footprint on any existing command until `ociboot install`/`ociboot
upgrade` actually start calling it.

## What's still not here

* Building directly from streamed OCI layers rather than only a
  materialized source directory tree.
* Deriving `timestamp`/`uuid` from a real manifest digest (`ociboot`'s
  own policy, deliberately not this crate's).
* A feature-gated pure-Rust writer as an alternative `ErofsBuilder`
  implementation.
* Sealing: fsverity on the built image, with a detached dm-verity hash
  tree as fallback where the state filesystem lacks fsverity support.
* Any of `ociboot`'s own subcommands (`install`, `upgrade`, boot flow)
  — this increment only gives the crate a real, working, verified
  first capability to build on.
