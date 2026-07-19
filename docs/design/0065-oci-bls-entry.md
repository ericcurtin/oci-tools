# Design note 0065: `oci-bls` Type #1 BLS entries (milestone 5)

Status: implemented (a single entry's own file content: parse/get/set/
push/serialize/read/write; scanning `/loader/entries/` as a directory
and the boot-counting filename-suffix convention are separate, later
follow-ups — see "What's still not here")
Scope: `crates/oci-bls/src/entry.rs` (new), `crates/oci-bls/src/lib.rs`.

Second real capability for `oci-bls` (0064 shipped grubenv read/write).
`ociboot`'s own planned deployment flow (milestone 5/6) writes one BLS
entry per deployment under `$BOOT/loader/entries/`; this increment is
the entry file format itself.

## Unlike grubenv, a real, authoritative, versioned spec exists

0064's grubenv had no written specification at all — grub2's own C
implementation was the only authority, so everything there was
verified by direct, hands-on comparison against the real binary. BLS
entries are different: the uapi-group publishes a real, versioned
specification (UAPI.1), fetched and read directly before writing any
of this. Its own complete worked example — reproduced verbatim in
`entry.rs`'s own `SPEC_EXAMPLE` test constant, comment line and column
alignment included — round-trips through [`parse`]/
[`Entry::to_string_repr`] with every key and value preserved exactly
(the reserialized form doesn't reproduce the example's own cosmetic
column alignment, which the spec explicitly says doesn't matter:
*"separated by one or more spaces"*).

## A generic, ordered representation — not five hardcoded fields

`oci-bls`'s own doc comment has named a deliberately narrow initial
scope since milestone 1 (`title`/`version`/`linux`/`initrd`/
`options`). Rather than hardcoding exactly those five fields and
nothing else, [`Entry`] stores every recognized `key value` line
generically, in declaration order, with named convenience accessors
for the common keys (`title`/`version`/`machine_id`/`sort_key`/
`linux`/`initrd`/`options`) layered on top of a generic
`get`/`get_all`/`push`/`set`. This isn't scope creep: an entry
`ociboot` *reads* (a coexisting installation on the same `$BOOT`,
or one `kernel-install` itself produced) may legitimately use keys
this crate has no named accessor for yet (`architecture`,
`devicetree`, `uki`, `extra`, ...), confirmed directly against the
real spec's own worked example, which itself uses `architecture` and
`extra`. Silently dropping those fields on a round trip would be a
real, observable correctness bug for anyone sharing `$BOOT` with
`ociboot`, not a cosmetic one — verified directly:
`parses_the_real_specs_own_worked_example` checks `architecture`
(not one of this crate's own named accessors) came through unchanged
via the generic `get`.

## Repeatable vs. non-repeatable keys, matching the real spec's own rules

The real spec says `initrd`/`options`/`extra`/`devicetree-overlay`
"may appear more than once", combined "in the order they are listed".
[`Entry::push`] appends without touching existing occurrences (for
these); [`Entry::set`] replaces the *first* existing occurrence of a
non-repeatable key (`title`, `version`, ...) in place, or appends a new
line if it wasn't present — deliberately simpler than 0064's
`GrubEnv::set`, which had to match a *real tool's own* observed
position-preserving behavior; there is no equivalent external
authority to match here (this crate is the only writer of its own BLS
entries), so the simplest correct behavior was chosen directly rather
than reverse-engineered.

## Same atomic-write reasoning as grubenv

[`write`] writes to a real temporary file in the same directory and
renames it into place — the same reasoning as 0064's `grubenv::write`:
a torn write to a boot menu entry (unlike grubenv, there isn't even a
real prior-art tool to compare non-atomicity against here, since this
crate is the only writer) is a genuinely bad place for a real machine
to be in, so this was built atomic from the start rather than as a
later improvement.

## Real, automated tests

9 tests in `oci-bls::entry`: the real spec's own worked example parses
correctly field-for-field, including a key this crate has no named
accessor for (`architecture`); a full parse-then-reserialize-then-
reparse round trip is a semantic no-op; comments and blank lines are
ignored; repeatable keys preserve declaration order; `set` replaces a
key in place rather than moving it to the end; a real file
write-then-read round-trips exactly; two successive real writes never
leave a partial/torn file on disk (checked by reading back after both);
an empty entry serializes to an empty string and vice versa; a
malformed line with no value is skipped rather than panicking.

## Performance

This increment touches only the still-small `oci-bls` crate —
`oci-runtime-core`/`ocirun`/`ociman run`'s own hot paths are untouched
(confirmed via `git diff --stat`), and `oci-bls` isn't linked into any
binary's own hot path yet either, so no benchmark re-verification was
needed. No new external dependency and no CI package-list change was
needed (pure Rust text parsing, no external tool involved at all,
unlike 0061's `mkfs.erofs` or 0063's `veritysetup`).

## What's still not here

* Scanning `$BOOT/loader/entries/*.conf` as a directory and building
  the sorted menu list the real spec's own "Sorting" section describes
  (`sort-key`, `machine-id`, version-order comparison).
* The boot-counting filename-suffix convention
  (`+tries_left-tries_done`) — a filename-rename-based mechanism per
  the real spec, deliberately not touching an entry's own file content
  at all, so it doesn't depend on anything in this increment beyond
  the file already existing.
* Building/writing an entry's own recommended companion directory
  layout (`$BOOT/<entry-token>/<version>/{linux,initrd}`).
* Everything else 0064 already listed as still ahead for the crate
  overall (atomic default-entry flips, boot counting proper, kargs
  editing, the `grub2-mkconfig`/`grub-install` traits).
