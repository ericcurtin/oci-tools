# Design note 0525: `ocibox export --bin --sudo`/`-S`

Status: implemented
Scope: `bin/ocibox/src/main.rs`, `tests/tests/ocibox_export.rs`.

## What this closes

Real `distrobox export --sudo`/`-S` had no `ocibox export` equivalent
at all -- a real CLI flag `ocibox` would reject as unrecognized.
Scoped to `--bin` only for this first slice; `--app` is a clear,
immediate error when combined with `--sudo` rather than silently
doing nothing.

## Real, checked-directly confirmation -- and two real, deliberate simplifications

- `~/git/distrobox/internal/inside-distrobox/assets/
  distrobox-export:105,154-157`: `--sudo`/`-S` registered.
- `~/git/distrobox/internal/inside-distrobox/assets/
  distrobox-export:270-296`: the real detection/priority logic --
  `sudo -S test` (probing passwordless capability) falling back to
  plain `sudo`; then `doas` (if present on `$PATH`) overrides that
  entirely; then `su-exec root` (if present) overrides even `doas`.
  Whichever wins gets baked as a literal string into the generated
  wrapper (`generate_script`, `distrobox-export:332,336`), right
  before the exported binary's own path.

This project's own `--bin` export is entirely host-side and static
(`cmd_export_bin`'s own `rootfs_bin.is_file()` -- a plain path check,
never a live command run inside the box), unlike real distrobox's
own script, which runs *from inside* the box and can genuinely probe
live capability/`$PATH`. Two real, deliberate, honestly-documented
simplifications follow directly from that:

1. Only plain `/usr/bin/sudo` is checked for, statically, inside the
   box's own rootfs -- `doas`/`su-exec` detection needs the identical
   live-probe machinery this project's export model doesn't have at
   all, and stays a real, separate, deliberately deferred gap.
2. The passwordless-`sudo -S`-capability probe can't be replicated
   statically either -- this project always uses plain `sudo`
   (matching real distrobox's own actual fallback whenever
   passwordless sudo isn't already configured, the common case for a
   freshly created box).

A box with no `/usr/bin/sudo` at all is a real, immediate, clear
error at export time -- a deliberate improvement over real
distrobox's own less defensive behavior there (which would still
generate a wrapper invoking a `sudo` that may not exist, only failing
confusingly later at actual invocation time), matching this project's
own already-established "fail clearly and early" convention
(`rootfs_bin.is_file()`'s own doc comment already gives the identical
reasoning for the exported binary itself).

## Implementation

`Command::Export` gains `sudo: bool` (`#[arg(long, short = 'S')]`).
`cmd_export` rejects `--sudo` combined with `--app` outright (a
clear, immediate error, not a silent no-op -- real distrobox's own
identical mechanism *does* apply to `--app` too, so silently
ignoring it here would be dishonest about what this project actually
supports). `cmd_export_bin` gains a `sudo: bool` parameter: when
true, checks `<box>/rootfs/usr/bin/sudo` exists (erroring clearly if
not), then inserts a literal `sudo ` right before the exported
binary's own single-quoted path in the generated wrapper's `exec`
line.

## Tests

Three new integration tests in `tests/tests/ocibox_export.rs`:
`export_bin_sudo_flag_prefixes_the_wrapper_with_sudo` (a real
`usr/bin/sudo` seeded into the box's rootfs, verifying the wrapper's
own exec line contains `sudo` in the right position),
`export_bin_sudo_without_sudo_installed_is_a_clear_error`, and
`export_app_and_sudo_together_is_a_clear_error`.

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (123
test-result blocks -- no new test file added, so the block count is
unchanged from `0524`; the documented transient `ocicri_container.rs`
flakiness under this host's own persistent CPU contention showed up
once, confirmed transient by rerunning the specific failing test in
isolation -- passed -- then a clean full-suite rerun), `python3
ci/guards.py` (clean), `cargo deny check` (clean), `bash
ci/native-ci.sh` (clean on the first attempt with
`RUST_TEST_THREADS=2`), `bash ci/build-deb.sh` (clean on the first
attempt, real `dpkg -i`/`--version`/`dpkg -r` round trip). An
export-time-only feature, not part of any launch/run hot path -- no
`ci/bench.sh` rerun needed.

## Deliberately still out of scope

`doas`/`su-exec` detection (needs the same live-probe machinery this
project's static export model has never had), the passwordless-
`sudo -S` capability probe (needs live execution too), and wiring
`--sudo` into `--app`'s own generated desktop entry (needs its own,
separate verification of exactly how a desktop entry's `Exec=` line
would embed it) all remain open candidates for future increments.
</content>
