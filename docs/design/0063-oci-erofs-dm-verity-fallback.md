# Design note 0063: `oci-erofs` detached dm-verity fallback (milestone 5)

Status: implemented (sealing/checking via plain files only; actually
mounting a dm-verity-protected image at boot is a separate, larger,
`ociboot-init`-owned follow-up — see "What's still not here")
Scope: `crates/oci-erofs/src/dmverity.rs` (new), `crates/oci-erofs/
src/lib.rs`, `crates/oci-erofs/Cargo.toml`, `ci/vm-prepare.sh`.

Third increment into milestone 5 (0061 shipped the `mkfs.erofs`
builder, 0062 shipped fs-verity sealing). `oci-erofs`'s own doc comment
has named this exact piece as planned scope since 0061: *"a detached
dm-verity hash tree as fallback when the state filesystem lacks
fsverity support"*.

## Why `veritysetup` here, unlike fs-verity's own direct ioctls

0062 deliberately used raw ioctls instead of a shellout because
fs-verity is just two simple kernel operations. dm-verity is a
different shape entirely: building a real Merkle hash tree file and
computing its root hash is genuine, nontrivial cryptographic work, not
a single syscall — exactly the kind of thing `docs/HACKING.md`'s own
sanctioned-shellout list carves out `veritysetup` for, the same way
0061 shells out to `mkfs.erofs` rather than hand-rolling an erofs
writer.

## Everything here stays at the plain-file level — confirmed directly, not assumed

The obvious assumption going in was that dm-verity, being a
device-mapper mechanism, would need loop devices and `/dev/mapper`
activation just to build and check a hash tree. Checked directly
first: `veritysetup format <data> <hash>` and `veritysetup verify
<data> <hash> <root_hash>` both work perfectly well against two
ordinary regular files, as a plain unprivileged user, with no loop
device and no device-mapper target ever created. That's genuinely
useful: sealing a build output and later checking it for corruption
(this crate's own actual scope) never needs privilege at all. Only
*mounting* a dm-verity-protected image at boot — presenting a new
verified block device other tools can mount from — needs the full
loop-device-plus-device-mapper machinery, and that's `ociboot-init`'s
own later, much larger, genuinely-privileged boot-time-flow concern,
not this crate's.

## Reproducibility: `veritysetup format` defaults to random, same as `mkfs.erofs` would without explicit options

Confirmed directly: `veritysetup format` with no `--uuid`/`--salt`
generates a fresh random one of each on every run, which would make
the resulting hash tree — and therefore its own root hash — different
every time even for byte-identical input data. `FormatOptions` has
both fields mandatory (no `Default` impl), matching
`builder::BuildOptions`'s own established convention in this crate:
built two hash trees for the same data more than a second apart in
wall-clock time, with a fixed `--uuid`/`--salt` both given explicitly,
and confirmed both the resulting hash tree bytes and root hash matched
exactly.

## Getting the root hash out without parsing free-form text

`veritysetup format`'s own human-readable summary
(`Root hash:      \t<hex>`) isn't a stable machine interface to depend
on. `--root-hash-file=<path>` is: it writes just the bare hex string,
nothing else, to a file of the caller's choosing. [`format`] uses a
real `tempfile::NamedTempFile` for this (confirmed: `veritysetup`
creates the hash-tree file itself if it doesn't already exist, so no
similar pre-creation dance is needed for that one), reads it back, and
lets it clean itself up.

## `verify`'s three real, distinct outcomes

Confirmed by direct reproduction, not assumed from the man page:
`veritysetup verify` exits `0` for a genuine match; exits `1` for a
root hash that simply doesn't match the hash tree; exits `2` when the
hash tree correctly detects real, mid-tree data corruption; exits `4`
(and others) for a real setup problem, e.g. a missing data file. Exit
codes `1`/`2` are mapped to `Ok(VerifyOutcome::Invalid)` — a real,
expected, checkable outcome a caller needs to act on (e.g. refuse to
boot a corrupted deployment), not something that should force every
caller to match on error text or conflate with a genuine environment
failure. Anything else stays `Err`.

## Real, automated tests

5 new tests in `oci-erofs::dmverity`, all against the real
`veritysetup` binary (skipping themselves with a clear message if it
genuinely isn't installed, matching `oci-erofs::builder`'s own
pattern; none need `sudo`, confirmed to run as a plain unprivileged
user): format-then-verify round-trips successfully and the root hash
is checked to actually look like a real sha256 hex digest (64 hex
chars); the same data formatted twice, more than a second apart,
produces byte-identical hash trees and matching root hashes; a
single-byte corruption introduced *after* sealing is caught as
`VerifyOutcome::Invalid`, not a Rust error; an intentionally wrong root
hash is likewise `Invalid`, not an error; a missing data file is a real
`Err`, distinguishing "the operation itself couldn't run" from "the
operation ran and found a mismatch".

## CI: `cryptsetup`/`cryptsetup-bin` added to both VM base images

`veritysetup` ships as part of the `cryptsetup` package on CentOS
Stream 10's `dnf` repos and `cryptsetup-bin` on Ubuntu 26.04's `apt`
repos (confirmed against this development host's own installed
package, `dpkg -L cryptsetup-bin` lists `/sbin/veritysetup`
specifically) — added to `ci/vm-prepare.sh` alongside 0061's
`erofs-utils`, so these tests run for real inside CI, not just
locally.

## Performance

This increment touches only the still-brand-new `oci-erofs` crate and
`ci/vm-prepare.sh` — `oci-runtime-core`/`ocirun`/`ociman run`'s own hot
paths are untouched (confirmed via `git diff --stat`), and `oci-erofs`
still isn't linked into any binary's own hot path (nothing outside its
own tests calls it yet), so no benchmark re-verification was needed.

## What's still not here

* Actually mounting a dm-verity-protected image at boot
  (`veritysetup open` against loop devices, presenting a new
  `/dev/mapper/<name>` device) — a later, genuinely privileged,
  `ociboot-init`-owned boot-time-flow increment.
* Wiring either sealing mechanism (this or 0062's fs-verity) into an
  actual `ociboot install` flow that decides which one to use based on
  the real state filesystem's own capabilities.
* Everything else 0061/0062 already listed as still ahead (streamed-
  layer building, manifest-digest-derived `timestamp`/`uuid`/`salt`, a
  pure-Rust `mkfs.erofs` alternative).
