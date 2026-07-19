# Design note 0087: `ociboot list` (milestone 5)

Status: implemented
Scope: `bin/ociboot/src/main.rs` (new `list` subcommand, `cmd_list`,
`boot_count_status`), `bin/ociboot/Cargo.toml` (new `oci-bls`
dependency), `tests/tests/ociboot_list.rs`.

`ociboot` has been a pure skeleton since milestone 1 — `clap` global
flags only, `main` unconditionally `bail!`ing "no subcommands are
implemented yet." Meanwhile `crates/oci-bls` (milestone 5, docs
0064/0065/0070/0071) had already built and thoroughly tested every
primitive needed to read and order a real boot menu: `scan_entries`
(read every `*.conf` in a real `$BOOT/loader/entries/` directory,
tolerating whatever else the real spec allows to coexist there),
`sort_entries` (the real spec's own four-rule "Sorting" section, bad
boot-counted entries last, `sort-key`/`machine-id`/version priority,
file-name fallback), and `parse_suffix` (the real
`+tries_left-tries_done` boot-counting file-name convention). 0071's
own "What's still not here" section named this exact gap directly:
*"Wiring `scan_entries` + `sort_entries` together into an actual
`ociboot`-facing 'list the boot menu in order' function."* This
increment is exactly that — `ociboot`'s first real subcommand.

## What it does

`ociboot list --boot-dir <DIR>` (default `/boot/loader/entries`, the
real, conventional path for a BLS-compliant boot loader on a modern
distro) scans `DIR`, sorts the result with `oci_bls::sort_entries`,
and prints one line per entry in the real would-boot order: title,
version (if set), and a boot-counting status suffix (` [bad]` or
` [tries left: N]`) for any boot-counted entry, matching the same
stem-stripping `oci_bls::sort::is_bad`'s own (private) helper already
does internally — duplicated here as a small, self-contained function
rather than exposing a new public API in `oci-bls` just for this one
caller. An empty directory prints a clear "no boot entries found"
message and still exits successfully (an installation with no boot
entries yet isn't an error); a directory that can't even be listed at
all (doesn't exist, no permission) is a real, surfaced error via
`anyhow::Context`.

## Zero new logic in the primitive crates

This is purely a wiring exercise: no changes to `oci-bls` itself at
all. Every behavior `ociboot list` exhibits (sort order, boot-counting
tolerance, non-`.conf`-file tolerance) is already independently unit-
tested inside `oci-bls`'s own test suite; this increment's own tests
exercise the CLI surface (the built binary, real argument parsing,
real process exit codes and stdout), not the sorting/parsing logic a
second time.

## Real, manual verification against the real spec's own worked examples

Built the release binary and ran it against a synthetic directory
built from the real UAPI Boot Loader Specification's own worked
example entries (`6a9857a3-3.8.0-2.fc19.x86_64.conf` and a newer
`...-3.9.0-1...` sibling, plus a `rollback+0.conf` boot-counted "bad"
entry and an unrelated `README.txt`) — confirmed the newer entry
prints first, the bad entry sorts last (`[bad]`) despite having the
lowest version number of the three, and the stray `README.txt` is
silently ignored. Also confirmed the empty-directory and missing-
directory cases print/exit correctly.

## Real, automated tests

Six integration tests in `tests/tests/ociboot_list.rs`, run against
the actual built `ociboot` binary: real spec sort order (file-name
fallback, since neither entry sets `sort-key`), a boot-counted bad
entry sorting last regardless of version, non-`.conf` clutter
tolerance, an empty directory's own message, a missing directory's
real error, and giving no subcommand at all being a real error rather
than a silent success.

## Not a hot-path change — no A/B perf re-verification needed

Touches only `bin/ociboot`, a binary this project doesn't benchmark
at all (the two explicitly benchmarked binaries are `ocirun`/`ociman`)
and doesn't call into `oci-runtime-core`, `synthesize_spec`,
`command_for`, or either cgroup driver at all.

## What's still not here

* This dev host doesn't actually have a real `/boot/loader/entries`
  directory (classic grub.cfg, not grub2-bls) — manual verification
  used a synthetic directory built from the real spec's own worked
  examples instead, same approach `oci-bls`'s own unit tests already
  use.
* `grubenv`'s own `saved_entry`/default-entry concept (which entry a
  real boot loader would actually boot *next*, as opposed to just the
  ordered list) is deliberately not surfaced here — `grubenv`'s real
  on-disk location varies by BIOS/UEFI/distro layout in a way this
  increment didn't need to pick a default for; a future increment can
  add it once `ociboot`'s own install/upgrade flow needs to read/write
  it anyway.
* Atomic default-entry flips, the `boot_success`/
  `boot_indeterminate_count` grubenv protocol, kernel argument (kargs)
  editing, `install`/`upgrade`/`switch`/`rollback`/`gc`, the `/etc`
  three-way merge — all still exactly as 0071 left them; this
  increment only gives `ociboot` its first real, read-only subcommand.
