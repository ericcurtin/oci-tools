# Design note 0200: `ociboot build-image` + shared rootfs-cache root

Status: implemented
Scope: `crates/oci-store/src/rootfs_cache.rs`/`lib.rs` (new, shared
`cache_root`); `bin/ociman/src/rootfs_setup.rs` (delegates to it);
`bin/ociboot/Cargo.toml` (new `oci-store`/`oci-erofs`/`oci-spec-types`
dependencies); `bin/ociboot/src/main.rs` (`open_store`, new
`Command::BuildImage`, `cmd_build_image`,
`deterministic_uuid_from_digest`); `tests/tests/ociboot_build_image.rs`.

## A deliberate pivot, checked against the goal first

The last several increments (0192-0199) were all narrow `ociman
build`/`ociman prune` CLI-flag additions. A periodic self-check-in
this turn re-read the README and confirmed: milestone 4 remains
genuinely "in progress" and each of those was individually well-scoped
and real, but nine consecutive turns on one command's own flag surface
while two entire binaries (`ocicri`, `ocibox` — milestone 7) sit as
untouched 37-line skeletons, and milestone 5 (`ociboot`, already
"in progress" per the README's own milestone table, *before* 4 even
finishes) had grown real, well-tested library primitives (`oci-erofs`,
`oci-bls`, `oci-mount`) with only two thin CLI subcommands
(`list`/`grubenv`) ever wired on top of them, is a real risk of
over-indexing on one binary at the expense of the stated goal that
*every* binary should be a working, benchmarkable drop-in replacement.

Milestone 7 (two steps past the current active front) was deliberately
*not* where this turn jumped to — that would have skipped milestone
6 entirely, a bigger deviation from the README's own stated order than
continuing 4/5 the project has already been doing (4 and 5 already run
concurrently "in progress" together). A full `ociboot install to-disk`
(real partitioning, bootloader installation, BLS entry writing) is
also too large and too risky (real destructive disk operations) for one
"small, safe, reversible" turn. This increment is instead the first
genuinely new, safe, well-scoped slice of milestone 5's own already-
active `install to-disk` deliverable: building the actual sealed
deployment image itself, deliberately stopping short of ever touching
a real disk, partition table, or bootloader.

## `cache_root` moved into `oci-store` (a pure, safe refactor)

`ociboot`'s new command needs the exact same already-extracted-rootfs
cache `ociman run`'s own overlay setup already shares across every
container of the same image (`oci_store::ensure_cached`) — reusing it
rather than building a second, independent extraction of the same
image saves real disk space and time, directly serving this project's
own "share as much Rust code as possible"/"ensure we don't run out of
disk space" standards. The one missing piece was `cache_root` itself
(`store.root().join("rootfs-cache")`), previously `pub(crate)` inside
`bin/ociman/src/rootfs_setup.rs`. Moved to `oci_store::rootfs_cache`
(re-exported at the crate root as `oci_store::cache_root`, alongside
the already-public `ensure_cached`/`cache_dir_for`/`prune`) — a pure,
zero-behavior-change move; `ociman`'s own existing `rootfs_setup::
cache_root` becomes a one-line delegate, so none of its own existing
call sites (`cmd_run`'s overlay setup, `cmd_prune`'s cleanup pass) had
to change at all. Verified via the full `oci-store`/`ociman` test
suites, unaffected.

## `ociboot build-image`

```
ociboot build-image <REFERENCE> --output <PATH> [--volume-label <LABEL>]
```

Deliberately *not* named to look like a real bootc-compatible
subcommand (`ociboot` is explicitly its own design, not a bootc CLI
mirror) — a clearly `ociboot`-specific name, distinct from the
eventual `install`/`upgrade`/`switch`/`rollback` surface still ahead,
so no future naming collision or confusion.

* Resolves `REFERENCE` against local storage only (`oci_spec_types::
  Reference::parse` first, matching every one of `ociman`'s own call
  sites — `Store::resolve_image` itself does only an exact string
  match, never normalizing on its own) — never pulls one itself; a
  clear "run `ociman pull` first" error if it isn't already present.
* `oci_store::ensure_cached` gets/builds a real, extracted rootfs for
  that image's own manifest digest — the same cache `ociman run`
  already populates and reuses.
* `oci_erofs::MkfsErofs::build` turns that rootfs into a real,
  deterministic erofs image at `--output`, via `oci_erofs::
  BuildOptions` whose two required-for-determinism fields are derived
  here (per `oci-erofs`'s own doc comment naming this "`ociboot`'s
  policy to own"):
  * `timestamp`: the image's own `created` field (0197), parsed and
    converted to epoch seconds — real, meaningful provenance (when
    this specific image was actually built) rather than an arbitrary
    number, while still fully deterministic (never wall-clock "now").
    Falls back to `0` if `created` is missing/unparseable.
  * `uuid`: the first 32 of the manifest digest's own 64 hex
    characters, regrouped into the standard 8-4-4-4-12 shape `mkfs.
    erofs -U` expects — not a real, versioned UUID (RFC 4122 v5 or
    otherwise), just a deterministic reformatting; the same manifest
    digest always yields the same UUID, and this project's own
    existing all-content-addressed-by-sha256 convention already makes
    two different digests colliding in their own leading 16 bytes
    exceedingly unlikely — the same practical guarantee already relied
    on elsewhere in this workspace.
  * `all_root: true` (`oci-erofs`'s own stated best practice).

Verified by hand against the real, installed `mkfs.erofs`: the output
is a real, valid erofs superblock (checked directly with `file`/
`dump.erofs`, matching the fixed-offset magic-number check `oci_erofs`'s
own unit test already uses), `dump.erofs`'s own decoded "Filesystem
created" line matches the seed image's own real `created` timestamp
exactly, and building the same image twice (with a real >1s delay in
between) produces byte-identical output — confirming the derivation is
genuinely deterministic, never wall-clock-dependent.

## Tests

Three new integration tests in `tests/tests/ociboot_build_image.rs`:
a real, valid erofs image gets written (fixed-offset magic-number
check); the same image built twice is byte-identical (with a real
delay between the two, catching any accidental wall-clock dependence);
an unknown reference is a clear error naming `ociman pull`. Full
`cargo build --workspace --locked`/`cargo test --workspace --locked`
(2 clean runs, 84/84 result blocks — one more than before, the new
test binary)/`cargo fmt --all --check`/`cargo clippy --workspace
--all-targets --locked -- -D warnings`/`python3 ci/guards.py`/`cargo
deny check`/`bash ci/native-ci.sh` all clean. No performance
regression (`ociman run --rm`, ~68ms, consistent with prior
measurements — this change doesn't touch any of `ociman run`'s own
call path beyond the pure `cache_root` rename).

## What this doesn't do yet

Real partitioning, bootloader installation (GRUB/systemd-boot config,
BLS entry writing for the newly-built deployment), fs-verity sealing
of the output image (`oci_erofs::verity::enable` — deliberately
deferred: needs a real verity-capable destination filesystem to
meaningfully test, a separate concern from just building the image
correctly), the `boot_success` grubenv protocol, actually mounting a
verified image at boot, and the dracut module are all still ahead —
this is genuinely just the "build a real, sealed-ready deployment
image from an OCI reference" slice of `install to-disk`, nothing more.
