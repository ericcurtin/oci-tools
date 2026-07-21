# Design note 0125: `ociboot grubenv` — a real, pure-Rust `grub-editenv`

Status: implemented
Scope: `crates/oci-bls/src/grubenv.rs` (`GrubEnv::entries` new
accessor); `bin/ociboot/src/main.rs` (`Command::Grubenv`,
`GrubenvAction`, `cmd_grubenv`); `tests/tests/ociboot_grubenv.rs` (new,
8 tests).

## Diversifying beyond `ociman`, deliberately narrowly

The last several sessions' worth of increments (0112-0124) were all
`ociman` polish (Dockerfile parity, build-cache correctness, image-ID
resolution). Genuinely valuable, and squarely in scope (`ociman` is one
of the five real drop-in-replacement binaries this project's own goal
names) — but milestone 5 (`ociboot`, `oci-erofs`, `oci-bls`,
`oci-mount`) had gone untouched for just as long, with `ociboot` itself
still only having its first subcommand (`list`, 0087). This increment
makes real, if narrow, progress there instead — chosen specifically
because it needed **no boot-time testing, no VM, no real disk** at
all: `oci-bls::grubenv` already had a complete, independently tested
read/set/unset/write implementation (verified byte-for-byte against
the real `grub-editenv` binary since its own first increment); nothing
in `ociboot` actually exposed it as a real, usable command yet.

## Why this specific piece, not the bigger `install`/boot-counting protocol

`oci-bls`'s own "planned scope, still ahead" list names three items:
atomic default-entry flips, the `boot_success`/`boot_indeterminate_
count` protocol, and kargs editing. All three need a real *policy*
decision this increment deliberately avoids: what value goes in
`saved_entry` for a real BLS-aware GRUB (a real, distro-convention-
dependent question with genuine correctness stakes — a wrong answer
here could mean a real machine fails to boot the intended deployment),
and exactly when/how the boot-counting protocol's own state machine
should transition. Those deserve their own careful, dedicated design
note, ideally checked against a real BLS-aware GRUB2 install (not
available on this development host, which boots via classic,
non-BLS GRUB menu entries — confirmed directly, `grub-set-default`'s
own `--help` describes exactly that older `MENU_ENTRY` convention, not
BLS ids). `ociboot grubenv` instead ships exactly what real
`grub-editenv`'s own scope already is: a **generic** key/value editor,
with no BLS-specific opinion about *which* keys matter or what their
values should be — the safe, narrow, already-fully-tested-underneath
mechanism the policy pieces above will eventually be built on.

## Matches real `grub-editenv`'s own CLI surface exactly

Checked directly (`grub-editenv --help` against the real, installed
binary on this host): `FILENAME {create|list|set NAME=VALUE...|unset
NAME...}`. `ociboot grubenv --file <path> <create|list|set|unset>`
mirrors this (a `--file` flag instead of a positional, matching this
project's own established CLI style elsewhere rather than a strict
character-for-character CLI clone) via `oci_bls::grubenv`'s own
already-existing `read`/`write`/`GrubEnv::{get,set,unset}`. The only
new code needed in `oci_bls` itself: a public `GrubEnv::entries()`
iterator (for `list` to print) — the underlying field was already
private to the module, deliberately never exposed for direct mutation
before this increment needed a read-only enumeration.

`set NAME=VALUE`'s own malformed-input handling matches the real
tool's exact error wording (`grub-editenv testenv set NOEQUALSSIGN`
against the real binary: `"error: invalid parameter NOEQUALSSIGN."` —
`ociboot`'s own: `"invalid parameter \"NOEQUALSSIGN\" (expected
NAME=VALUE)"`, checked directly before writing the code, not assumed).

## Real, automated tests — including direct cross-compatibility with the real binary

Manually verified first, byte-for-byte, before writing any automated
test: `ociboot grubenv create` and real `grub-editenv ... create`
produce identical files; the same after a real `set` with multiple
assignments; `ociboot` correctly reads a file the real tool wrote
directly, and vice versa.

Eight new tests in `tests/tests/ociboot_grubenv.rs`: `create`'s own
1024-byte blank block; `set`+`list` round-tripping in insertion order;
`set` on an existing key preserving its original position (not moving
it to the end); `unset` removing a variable; a malformed `set`
argument surfacing the real error wording; `list` on a missing file
being a real, surfaced error; and two direct cross-compatibility tests
against a real, installed `grub-editenv` (skipping themselves,
printing why, if it isn't on `$PATH` — not a hard CI dependency,
matching this project's own established `busybox_path`-style pattern)
comparing the exact on-disk bytes `ociboot` vs. the real tool produce
for `create` and for a real `set`. All 6 pre-existing `ociboot list`
tests and all 49 pre-existing `oci-bls` unit tests still pass
unmodified. Full `cargo build --workspace --locked`/`cargo test
--workspace --locked` (2 clean runs)/`cargo fmt --all --check`/`cargo
clippy --workspace --all-targets --locked -- -D warnings` all clean.

## What this doesn't do yet

* No BLS-specific policy at all — `saved_entry`'s own correct value
  convention, the `boot_success` protocol's own state machine, and
  kargs editing all remain exactly as unstarted as `oci-bls`'s own
  module doc already said, each still needing its own dedicated,
  carefully-verified increment (ideally against a real BLS-aware
  GRUB2 install, not available on this development host).
* `ociboot install`/`upgrade`/`switch`/`rollback` — still not started,
  milestone 5/6's own much bigger remaining pieces.
