# Design note 0070: `oci-bls` directory scanning + boot-counting filename suffix (milestone 5)

Status: implemented (`scan_entries` + the `+tries_left-tries_done`
filename convention; the real spec's own "Sorting" section and the
separate `boot_success`/`boot_indeterminate_count` *grubenv* protocol
are still ahead — see "What's still not here")
Scope: `crates/oci-bls/src/scan.rs` (new), `crates/oci-bls/
src/boot_count.rs` (new), `crates/oci-bls/src/lib.rs`.

`oci-bls`'s own doc comment has named both of these as one planned
bullet since 0065 ("scanning `/loader/entries/*.conf` as a directory
... and boot-counting's `+tries_left-tries_done` filename-suffix
convention"). They're implemented here as two small, independently
testable modules rather than one, since they're genuinely separate
concerns (reading a directory's contents vs. a specific file-naming
convention some of those files may or may not use) that happen to
serve the same eventual `ociboot` feature.

## `scan_entries`: tolerant by design, matching the real spec's own stated tolerance

The real UAPI.1 spec (already the authoritative source `entry.rs`
itself cites) says a boot loader "must be able to operate correctly if
files or directories other than `/loader/entries/` and `/EFI/Linux/`
are found in the top level directory" — this directory is explicitly
*not* `ociboot`'s exclusive territory; other tools, or a coexisting
non-`ociboot` installation sharing the same `$BOOT`, may leave other
things in it. `scan_entries` matches that: anything that isn't a plain
`.conf` file, or a `.conf` file that exists but can't actually be
opened and read (a race with something else removing it, a
permissions problem, non-UTF-8 content), is silently skipped rather
than aborting the whole scan. `std::fs::read_dir` itself failing (the
directory doesn't exist at all, no permission to list it) stays a
real, surfaced `io::Error` — genuinely different from "one entry among
many is odd".

## `BootCount`: every rule checked against the real spec's own worked examples, not inferred from a summary

The real spec's own "Boot counting" section was fetched and read
directly (the same source already cited for the entry file format and
the directory-scanning rule above) — every behavior here traces to a
specific sentence in it, not a guessed summary:

* A file name may end `+<tries_left>[-<tries_done>].conf`, immediately
  before the extension; `tries_done` is implicitly zero if the
  `-<...>` part is missing entirely (`parse_suffix` represents that
  exact distinction as `tries_done: None`, not `Some((0, 0))`).
* Decrementing `tries_left` preserves its own digit width via
  zero-padding — the spec's own literal example, `+10` becoming `+09`
  rather than `+9`, is reproduced verbatim as a test.
* Incrementing `tries_done` is capped, not wrapped, at the maximum
  value its own recorded digit width can represent — the spec's own
  literal example, capped at `99` for a two-digit field, is likewise
  reproduced verbatim as a test.
* `tries_left` reaching zero marks an entry "bad" (`is_bad`), used by
  the spec's own "Sorting" section (still a later increment — see
  below) to sort bad entries last.

One real case the spec text leaves genuinely unspecified, called out
honestly rather than silently invented: what starting width to use if
`increment_tries_done` is called on an entry whose `tries_done` was
never tracked at all (`None`) — real usage always initializes both
counters together in practice, so this is a corner the spec simply
doesn't need to resolve. This project's own choice (start at `(1, 1)`,
one digit) is documented plainly as this project's own decision, not
attributed to the spec.

## Real, automated tests

4 new tests in `oci-bls::scan` (only real `.conf` files are scanned,
everything else in the directory tolerated — a stray text file and a
subdirectory both present at once; a boot-counted entry's own file
name, suffix included, is preserved verbatim; an empty directory scans
to no entries; a missing directory is a real `io::Error`). 11 new
tests in `oci-bls::boot_count`: the real spec's own two worked
examples parse correctly (`+10` with no `tries_done` part; `+3-0` with
both); no suffix at all means "not boot-counted", distinct from a real
`+0` suffix (a genuinely "bad" entry, still a real suffix); decrement
reproduces the spec's own `+10` → `+09` example exactly; decrement
saturates at zero rather than wrapping; increment reproduces the
spec's own `99`-cap example exactly; increment from `None` starts at
`(1, 1)`; `format_suffix` round-trips through `parse_suffix`; three
malformed-shape cases (`foo+`, `foo+3-`, `foo+abc`) are all correctly
not recognized as a real suffix rather than panicking.

## Performance

This increment touches only the still-growing `oci-bls` crate —
`oci-runtime-core`/`ocirun`/`ociman run`'s own hot paths are untouched
(confirmed via `git diff --stat`), and `oci-bls` still isn't linked
into any binary's own hot path yet either, so no benchmark
re-verification was needed. No new external dependency either
(`std::fs::read_dir` and plain string parsing, no regex or path-glob
crate).

## What's still not here

* The real spec's own "Sorting" section — ordering `scan_entries`'s
  own results by `sort-key`/`machine-id`/version-order comparison
  (needing the separate Version Format Specification's own comparison
  algorithm), with bad entries sorted last via `BootCount::is_bad`.
* The `boot_success`/`boot_indeterminate_count` *grubenv* protocol —
  genuinely different from this increment's own filename-based
  counting: real systemd's own Automatic Boot Assessment tracks a
  *global* current-boot outcome via those two grubenv keys
  (`crate::grubenv`, 0064), separate from any one entry's own filename
  suffix.
* Atomic default-entry flips, kernel argument editing, the
  `grub2-mkconfig`/`grub-install` traits — all still exactly as 0065
  left them.
