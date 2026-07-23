# Design note 0201: `ociboot build-image --seal`

Status: implemented
Scope: `bin/ociboot/src/main.rs` (`Command::BuildImage`'s new `--seal`
flag, `cmd_build_image`'s sealing step, `hex_encode`);
`tests/tests/ociboot_build_image.rs`.

## Continuing milestone 5

0200's own "what this doesn't do yet" named fs-verity sealing of the
built deployment image as the natural next step (`oci_erofs::verity`
already exists and is thoroughly unit-tested; the only missing piece
was wiring it into `ociboot build-image` itself).

## Design: opt-in, not automatic

fs-verity is a real, kernel-enforced, one-way "this file can never be
modified again" operation (`oci_erofs::verity::enable`'s own doc
comment: "fs-verity has no 'disable' operation by design"). Real
bootc/composefs use it unconditionally, but that project always writes
to a real target installation disk (almost always a real fs-verity-
capable filesystem). This project's own `--output` destination has no
such guarantee — plenty of realistic destinations (`/tmp` on many
development/CI hosts, an overlayfs or tmpfs mount) don't support
fs-verity at all. Making sealing the unconditional default would make
the command's own basic, most common usage fail outright on those.
Made it opt-in (`--seal`) instead: without it, exactly 0200's own
existing behavior (an ordinary, writable erofs image); with it, a
destination that doesn't support fs-verity is a clear, real error (the
kernel's own `EOPNOTSUPP`, surfaced via `.with_context`) rather than a
silent no-op — asking for sealing and silently not getting it would be
a false sense of security, never acceptable for something this
security-relevant.

## The fix

After the erofs image is written (and only after — sealing always
happens last, matching fs-verity's own one-way nature: nothing should
ever try to seal a not-yet-fully-written file), `--seal` calls:

1. `oci_erofs::verity::enable(output)` — seals the file.
2. `oci_erofs::verity::measure(output)` — reads back the real digest
   fs-verity just computed, then printed as `verity: <64 hex chars>`
   on its own line after the existing path line. A small local
   `hex_encode` (32 fixed bytes, no new dependency needed for
   something this simple — the same reasoning already established
   elsewhere in this workspace for small, self-contained encodings).

## Verified by hand

Built a real, fs-verity-capable ext4 loopback image (`mkfs.ext4 -O
verity`, the same fixture `oci_erofs::verity`'s own unit tests already
use), mounted it, and ran `ociboot build-image --seal` against a path
inside it:

* The printed digest is a real 64-hex-character (32-byte) value, never
  all-zero.
* The output file is genuinely immutable afterward — a real
  `>> file` append from the shell fails with `Operation not permitted`
  at the kernel level.
* Running the exact same command again (attempting to rebuild the now-
  sealed file in place) fails naturally at the `mkfs.erofs` step itself
  (it can no longer write to the sealed destination) — no special
  handling needed, the kernel-level immutability already produces the
  correct, honest failure on its own.
* Without `--seal`, the file stays perfectly ordinary and writable —
  confirms the flag is genuinely opt-in, not accidentally always-on.

## Tests

Two new integration tests in `tests/tests/ociboot_build_image.rs`,
replicating `oci_erofs::verity`'s own private `VerityFs` loopback-ext4
fixture (a small, deliberate duplicate — it's that crate's own private
test helper, not a public API, matching this project's own established
"small harmless duplication over new cross-crate test-only coupling"
precedent):
`build_image_seal_makes_the_output_genuinely_immutable_and_prints_a_real_digest`
(the real digest, and a real, kernel-enforced write failure afterward)
and
`build_image_without_seal_prints_no_verity_line_and_stays_writable`
(confirming the default path is completely unaffected). All 3
pre-existing `ociboot build-image` tests continue to pass unchanged (5
total now).

Full `cargo build --workspace --locked`/`cargo test --workspace
--locked` (2 clean runs, 84/84 result blocks each — one unrelated,
pre-existing, timing-sensitive flake in `ociman_logs`'s own `logs -f`
test was investigated directly: passed 6/6 times in isolation,
confirmed as transient noise from a real `elapsed >= 400ms` wall-clock
assertion under heavy full-workspace parallel test load, not a
regression from this change, which touches none of that code
path)/`cargo fmt --all --check`/`cargo clippy --workspace --all-targets
--locked -- -D warnings`/`python3 ci/guards.py`/`cargo deny check`/
`bash ci/native-ci.sh` all clean. No performance regression (`ociman
run --rm`, ~66ms, consistent with prior measurements — this change
touches only `ociboot`'s own new code, nothing on `ociman`/`ocirun`'s
own call path).

## What this doesn't do yet

The `boot_success` grubenv protocol, actually mounting a verified image
at boot, real partitioning/bootloader installation, a detached
dm-verity fallback wired into `ociboot` itself (the crate-level
primitive, `oci_erofs::dmverity`, already exists but isn't called from
here — for a destination filesystem that supports neither fs-verity
nor is a fresh loopback image `dmverity::format` could seal instead),
and the dracut module are all still ahead.
