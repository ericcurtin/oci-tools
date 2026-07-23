# Design note 0220: `ociboot build-image` writes a real deployment origin record

Status: implemented
Scope: `bin/ociboot/src/origin.rs` (new); `bin/ociboot/src/main.rs`
(`cmd_build_image`); `bin/ociboot/Cargo.toml`.

## Milestone 6 was completely untouched

`upgrade/switch/rollback/status/gc; /etc merge; boot counting; layered
mode` — before this increment, zero work had landed on it. A direct
research pass against real `bootc`'s own `status` implementation
(`~/git/bootc`, `bootc_composefs::status`) before starting anything
found the actual prerequisite: real `bootc status` reports which OCI
image reference/digest/version produced each deployment by reading a
real, on-disk `<digest>.origin` ini file it writes at deployment time
— and this project's own `ociboot build-image` (milestone 5) wrote
*no* such record at all. `oci_store`'s own `ImageRecord` tracks local
image storage, not deployment provenance; nothing anywhere in this
repo linked a built erofs image back to the OCI reference it came
from. A `status` command invented today, with no such record to read,
could only ever guess or fabricate — worse than not existing at all.

This increment closes exactly that one gap, and nothing else: writing
a real, deterministic provenance record next to every image
`build-image` produces. Reading it back (a real `ociboot status`) is
deliberately left for its own future increment.

## Not a literal port of bootc's own ini format

`ociboot` has its own design (this project's README says so
explicitly) — a plain JSON sidecar (`<output>.origin.json`) matches
this workspace's own already-established convention instead
(`oci_store::images`'s own `ImageRecord` pointer files, every
`--json` CLI output elsewhere), rather than inventing a second,
ini-flavored format just to mirror bootc's own internal choice for a
concept its own backend (composefs, not this project's flat erofs +
fsverite/dm-verity design) already models differently anyway (a
`deployments/<digest>/` directory per deployment vs. this project's
own single caller-chosen `--output` path).

## What's recorded, and why each field

- `image_reference` — the fully normalized reference (`oci_spec_types
  ::Reference`'s own `Display` form), the same one `cmd_build_image`
  already uses to resolve the image.
- `image_digest` — the resolved manifest digest, the real,
  content-addressed identity, not just the human-chosen tag.
- `image_version` — the image's own declared `org.opencontainers.
  image.version` label, if it set one; a real `None`/`null` when it
  didn't (checked directly: `ContainerConfig::default()`, used by
  almost every existing test fixture, sets no labels at all — a
  fabricated placeholder here would be actively misleading).
- `built_at` — reuses the *exact same* `timestamp` value already
  computed for `oci_erofs::BuildOptions` (the image's own `created`
  field when parseable, `0` otherwise) rather than deriving it a
  second time, so the origin record and the erofs image's own
  superblock timestamp can never disagree.

## Written silently — internal bookkeeping, not a user-facing result

Confirmed directly against the existing test suite's own exact-stdout
assertions (`build_image_writes_a_real_valid_erofs_image` asserts
stdout is *exactly* the output path, trimmed, nothing else) that
printing anything about the new origin file would have broken an
already-passing test. Rather than loosen that assertion, this write is
silent — the same category as `oci_store`'s own pointer-file writes,
not a user-facing result the way `--seal`'s own `verity:`/`dm-verity:`
digest lines are (those report a real, security-relevant, opt-in
outcome the caller explicitly asked for; this is bookkeeping metadata
every build produces unconditionally).

## Atomic, matching this project's own established pattern

`origin::write` uses the exact same same-directory-temp-file-plus-
rename technique `oci_store::images::put`'s own doc comment already
established ("a reader never observes a partially written pointer
file") — not a new technique invented for this.

## Verified

- New unit tests in `bin/ociboot/src/origin.rs` (round-trip through a
  real file; a missing origin file is a real `None`, not an error;
  overwriting an existing one never leaves a stale value behind).
- Two new integration tests in `tests/tests/ociboot_build_image.rs`:
  a real build with a version-labeled seeded image produces a correct
  `<output>.origin.json` (reference/digest/version all independently
  verified against the same `oci_store::Store` the command itself
  used) while stdout stays exactly the output path (the silent-write
  claim, checked, not assumed); a real build with no version label at
  all records a real `null`, not a placeholder.
- Every one of the six pre-existing `ociboot_build_image.rs` tests
  still passes completely unmodified, confirming this is a pure,
  additive change.
- Full workspace: `cargo build`, `cargo test --workspace` (94/94
  result blocks, 0 failures — `ociboot`'s own unit-test block grew
  3→6, `ociboot_build_image`'s own integration-test block grew 6→8,
  matching exactly the new tests added, nothing else moved), `cargo
  fmt --check`, `cargo clippy --all-targets -- -D warnings`, `python3
  ci/guards.py` (18 capability groups, unaffected — no new shared
  crate, no bin-to-bin dependency), `cargo deny check` (only the
  pre-existing benign warning), `bash ci/native-ci.sh`, hyperfine
  perf sanity on `ociman run --rm` (no regression — this change never
  touches any code on that command's own hot path at all).

## What this doesn't do yet

Nothing reads this record back yet — `ociboot status` (a real,
honest reshaping of `ociboot list`'s own BLS-entry data plus, now,
this new origin record where a future `install`/`build-image`
invocation actually writes a BLS entry pointing at one) is real,
separate, still-ahead follow-up work, along with everything else
milestone 6 names: `upgrade`/`switch`/`rollback`/`gc`, `/etc` three-way
merge, the `grubenv`-based `boot_success`/`boot_indeterminate_count`
protocol (distinct from the filename-suffix boot counting `bless`
already handles), and layered mode.
