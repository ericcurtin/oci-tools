# Design note 0062: `oci-erofs` fs-verity sealing (milestone 5)

Status: implemented (SHA-256 enable/measure; the detached dm-verity
fallback is a separate, larger follow-up — see "What's still not
here")
Scope: `crates/oci-erofs/src/verity.rs` (new), `crates/oci-erofs/
src/lib.rs`, `crates/oci-erofs/Cargo.toml`.

The second increment into milestone 5 (0061 shipped the `mkfs.erofs`
builder): sealing a built erofs image with fs-verity, the next piece
`oci-erofs`'s own doc comment already named as planned scope.

## Why direct ioctls, not another shellout

`docs/HACKING.md`'s own sanctioned-shellout list names `mkfs.erofs`,
`mkfs.ext4`/`mkfs.xfs`, `veritysetup`, `grub2-mkconfig`/`grub-install`,
`dracut`, `sfdisk`/`blkid` — deliberately *not* an `fsverity` CLI
binary. That's a real, meaningful distinction, not an oversight:
`mkfs.erofs` writes an entire on-disk format no simple syscall could
produce, but fs-verity itself is just two ioctls
(`FS_IOC_ENABLE_VERITY`/`FS_IOC_MEASURE_VERITY`) the kernel already
exposes directly — exactly the kind of primitive this workspace
already prefers to call directly (`rustix`/`libc`) rather than
shelling out to a wrapper binary, matching `oci-runtime-core`'s own
`identity`/`namespaces` modules.

## A real, confirmed bug caught by manual verification before any test existed

The first hand-computed `FS_IOC_ENABLE_VERITY` value was wrong by one
hex digit — `0x4086_6685` instead of the correct `0x4080_6685` — a
mistake that is *especially* dangerous for a raw ioctl number: it
didn't panic, didn't refuse to compile, and didn't fail with any
obviously-meaningful error. It silently produced a request number the
kernel's ioctl dispatcher doesn't recognize for that file, so every
call returned a generic `ENOTTY`, indistinguishable at a glance from
"this filesystem genuinely doesn't support fs-verity" (a real,
expected, non-bug outcome this module has to handle anyway). This was
only caught because verification here always means *running the real
thing* before trusting it: a small standalone throwaway binary using
nothing but hand-written `libc::ioctl` calls succeeded against a real
fs-verity-capable loopback ext4 image, while the exact same operation
through this crate's own (buggy) code failed — proving the bug was in
this crate, not the environment or the general approach.

Fixed two ways, not just one: the ioctl numbers are now *computed* via
a small `const fn ioc(dir, type, nr, size)` directly from each real
ioctl's own definition (`_IOW('f', 133, fsverity_enable_arg)` /
`_IOWR('f', 134, fsverity_digest)`) rather than a single hand-typed hex
literal, and a permanent unit test
(`ioctl_numbers_match_the_real_kernel_uapi_header`) checks the result
against the two independently-published magic numbers every real
fs-verity tool agrees on — specifically so a bug in the `ioc` helper
itself (or in either `#[repr(C)]` struct's layout) can't cancel out
and pass unnoticed the same way a single hardcoded literal did.

## Three real, distinct outcomes a caller needs to tell apart

All three confirmed directly against a real fs-verity-capable ext4
loopback image (`mkfs.ext4 -O verity`), not assumed from kernel docs
alone:

1. **Not sealed yet** (`measure` on a file where `enable` was never
   called) — the kernel's own `ENODATA`, mapped here to `Ok(None)`
   rather than an error, since this is an ordinary, expected state a
   caller checking a previous `ociboot install` run's own progress
   needs to handle without matching a raw errno itself.
2. **Already sealed** (`enable` called twice) — the kernel's own
   `EEXIST`, which `std` already categorizes as
   `io::ErrorKind::AlreadyExists`, passed straight through.
3. **This filesystem doesn't support fs-verity** — confirmed to appear
   as *either* `EOPNOTSUPP` (`io::ErrorKind::Unsupported`; a plain
   `mkfs.ext4` image without the verity feature bit) *or* `ENOTTY`
   (`std` has no dedicated `ErrorKind` for this one, checked via raw
   `errno` instead; confirmed directly on this very development host
   that some non-ext4/f2fs filesystem types return this instead) —
   both real, both need tolerating by this crate's own planned
   dm-verity fallback, so both are tested for explicitly rather than
   assuming only one ever occurs.

## `enable` always queries the real block size; never assumes one

fs-verity requires `block_size` in `fsverity_enable_arg` to exactly
match the containing filesystem's own actual block size, or the
kernel rejects the request outright with `EINVAL` — confirmed directly
by reproducing the failure: a 16 MiB ext4 image deliberately built
with `-b 1024` rejects a hardcoded `4096` request. [`enable`] queries
`fstatfs`'s own `f_bsize` on the real open file descriptor every time,
never assuming any fixed value.

## Real, automated tests

All against a real, freshly-built fs-verity-capable ext4 loopback
image (built and mounted for real inside the test itself via
`mkfs.ext4 -O verity`/`sudo mount`, then `chown`ed back to the calling
user so [`enable`]/[`measure`] are exercised exactly as `ociboot` would
really call them — unprivileged, ordinary file operations, not
root-only; `sudo` works passwordlessly both on this development host
and in the CI VM's own `ci` user per `ci/vm.sh`'s own cloud-init
`NOPASSWD:ALL`): enabling then measuring returns a real, non-zero
digest that is confirmed to differ from a plain `sha256sum` of the
same content (the fs-verity digest hashes a `fsverity_descriptor`
Merkle-tree structure, never raw content, by design); a sealed file
remains readable but a subsequent write genuinely fails with
`EPERM`/`PermissionDenied`, a real kernel-enforced guarantee, not just
this crate's own convention; enabling twice returns the real
`AlreadyExists`; enabling on a filesystem without the feature returns
one of the two real "unsupported" outcomes above (or tolerates success
outright, for a test environment whose own `/tmp` genuinely does
support it); plus the two permanent ioctl-number/struct-layout guard
tests described above. Each loopback-mount-dependent test skips itself
with a clear message if any setup step fails, matching the
`oci-erofs::builder` tests' own existing pattern for `mkfs.erofs` not
being installed.

## Performance

This increment touches only the still-brand-new `oci-erofs` crate —
`oci-runtime-core`/`ocirun`/`ociman run`'s own hot paths are untouched
(confirmed via `git diff --stat`), and `oci-erofs` still isn't linked
into any binary's own hot path (nothing outside its own tests calls
it yet), so no benchmark re-verification was needed.

## What's still not here

* A detached dm-verity hash tree (`veritysetup`, already named in
  `docs/HACKING.md`'s own sanctioned-shellout list) as a fallback for
  state filesystems that don't support fs-verity at all.
* Wiring either sealing mechanism into an actual `ociboot install`/
  `ociboot-init` boot-time verification flow — this increment only
  gives the crate a second real, working, verified capability to
  build on, alongside 0061's `mkfs.erofs` builder.
* Everything else 0061 already listed as still ahead (streamed-layer
  building, manifest-digest-derived `timestamp`/`uuid`, a pure-Rust
  `mkfs.erofs` alternative).
