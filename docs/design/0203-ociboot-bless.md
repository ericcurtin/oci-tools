# Design note 0203: `ociboot bless`

Status: implemented
Scope: `bin/ociboot/src/main.rs` (`Command::Bless`, `cmd_bless`);
`tests/tests/ociboot_bless.rs`.

## Continuing milestone 5

0202's own "what this doesn't do yet" named the `boot_success`
protocol as a remaining gap. Before implementing anything, fetched and
read the UAPI Boot Loader Specification's own "Boot counting" section
directly (already cited by `oci_bls::boot_count`'s own module doc
comment, but not previously quoted in full) rather than assuming which
half of the mechanism this project still owed:

> The main idea is that when boot entries are initially installed,
> they are marked as "indeterminate" and assigned a number of boot
> attempts. Each time the boot loader tries to boot an entry, it
> decreases this count by one. If the operating system considers the
> boot as successful, it removes the counter altogether and the entry
> becomes "good". [...] Which boots are "successful" is determined by
> the operating system.

This settles a real, easy-to-get-wrong scope question: the
*decrement* on every boot attempt is the real boot loader's own job
(`grub2-bls`/`systemd-boot`'s own internal C code — already running,
already correct, long before `ociboot`/`ociboot-init` are ever
invoked) — reimplementing it inside `ociboot` would be redundant, not
missing functionality. The one piece the spec explicitly assigns to
"the operating system" is confirming success and permanently disabling
counting for that entry — exactly, and only, what this increment
implements.

## The fix

New `ociboot bless --entry <FILE>`: reads the entry file's own name,
parses its boot-counting suffix via the already-existing, already-
tested `oci_bls::parse_suffix`, and — if present — renames the file to
strip the suffix entirely (`std::fs::rename`, the exact mechanism the
spec's own text prescribes: "removes the counter altogether"). If the
entry has no counting suffix at all (already "good", or never counted
to begin with), a harmless, clearly-reported no-op rather than an
error — genuinely idempotent, matching this project's own established
preference for a command whose whole point is "make sure X holds" to
succeed quietly when X already does.

## Verified by hand

* A `deploy+3.conf`/`deploy+2-1.conf`/`deploy+0-3.conf` (tries-left-
  only, both counters, and an already-"bad" entry respectively) each
  bless correctly to a plain `deploy.conf`, content byte-for-byte
  unchanged.
* An already-uncounted entry, and blessing the same entry twice in a
  row, are both confirmed harmless no-ops, not errors.
* `ociboot list` against the same directory before/after confirms the
  blessed entry's own `[tries left: ...]` display marker (`ociboot`'s
  own pre-existing `boot_count_status` helper) disappears once
  blessed, exactly as expected.

## Tests

Six new integration tests in `tests/tests/ociboot_bless.rs`: stripping
a tries-left-only suffix, stripping both counters, stripping an
already-"bad" entry's own suffix, a no-op for an already-good entry, a
no-op for blessing twice in a row (idempotence), and a clear error for
a nonexistent entry file.

Full `cargo build --workspace --locked`/`cargo test --workspace
--locked` (2 clean runs, 85/85 result blocks — one more than before,
the new test binary)/`cargo fmt --all --check`/`cargo clippy
--workspace --all-targets --locked -- -D warnings`/`python3
ci/guards.py`/`cargo deny check`/`bash ci/native-ci.sh` all clean. No
performance regression (`ociman run --rm`, ~67ms, consistent with
prior measurements — this change touches only `ociboot`'s own new
code, nothing on `ociman`/`ocirun`'s own call path).

## What this doesn't do yet

Real invocation of `ociboot bless` from a systemd unit (or equivalent)
running late in a real boot, once the OS is actually confirmed
healthy — this increment is the primitive the eventual unit would
call, not the unit itself. Actually mounting a verified image at boot,
real partitioning/bootloader installation, a real `--karg` flag on a
future `install`/`upgrade`, `ociboot`'s other subcommands, and the
dracut module are all still ahead.
