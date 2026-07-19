# Design note 0071: `oci-bls` version comparison + entry sorting (milestone 5)

Status: implemented (the full real "Sorting" section, built on a full
UAPI.10 version-comparison implementation)
Scope: `crates/oci-bls/src/version.rs` (new), `crates/oci-bls/
src/sort.rs` (new), `crates/oci-bls/src/lib.rs`.

0070 shipped directory scanning and the boot-counting filename
convention but explicitly deferred the real Boot Loader Specification's
own "Sorting" section, which needs a whole separate specification
(UAPI.10, Version Format Specification) for its own version-order
comparison. This increment implements both: the version-comparison
algorithm itself, and the sorting rules built on top of it.

## `version::compare`: a direct, line-by-line translation of the real spec's own eight numbered steps

UAPI.10 was fetched and read directly — its own text says the
algorithm "is based on rpm's `rpmvercmp()`, but not identical" and
spells out eight numbered comparison steps precisely.
`crates/oci-bls/src/version.rs`'s own `compare` function is a direct
translation of those eight steps in order (skip insignificant
characters; `~` always sorts lower; end-of-string; `-` always sorts
lower; `^` always sorts higher; `.` always sorts lower; numeric
prefixes compared numerically, empty evaluates as zero; alphabetic
prefixes compared letter-by-letter), not a re-derivation from the
one-line summary or from prior familiarity with `rpmvercmp` itself. One
implementation detail worth noting: step 8's own "capital letters
compare lower than lower-case (`B < a`)" and "a shorter prefix compares
lower than a longer one it's a prefix of" both fall out of plain ASCII
byte/lexicographic `str` comparison for free (`'B'` is `0x42`, `'a'` is
`0x61`), so no custom character-ranking logic was needed there at all.

## Verified two independent ways before trusting it: the spec's own text, and a real, separately-implemented tool

Every one of the real spec's own worked examples — both the short
standalone ones (`bar-123 < foo-123`, `11α == 11β`, `1_2_3 > 1.3.3`,
...) and the long ordered chain (`122.1 < 123~rc1-1 < 123 < ... <
124-1`) — is reproduced verbatim as a test. Before writing a single
line of Rust, every one of those same examples was also run directly
against the real `systemd-analyze compare-versions` binary (`systemd
255`, already installed on this development host) — the spec's own
"Notes" section names that exact tool as implementing this algorithm,
so agreement there is a second, independently-implemented confirmation
beyond the spec text alone, not merely re-checking the same source
twice.

## A pathological input handled correctly without needing a bignum dependency

Step 7's numeric comparison is normally a plain `u128::parse` (39
decimal digits of headroom — no real version string's own numeric
component ever gets close), but an absurdly long digit run (a
synthetic 50-digit test case, not a hypothetical real one) would
overflow that. Rather than either panicking or pulling in an
arbitrary-precision integer crate for a case that will essentially
never occur for real, `compare_numeric` falls back to comparing the
leading-zero-stripped digit strings by length first, then
lexicographically — correct for this specific case (digit strings of
equal length already sort in numeric order via plain byte comparison,
since `'0'`-`'9'` are consecutive ASCII code points) without adding a
new dependency for it.

## `sort_entries`: the real spec's own four rules, including "or are all equal" chaining rule 2 into rule 4

The real spec's own rule 2 (order by `sort-key`, then `machine-id`,
then decreasing `version`, when `sort-key` is set on both) and rule 4
(fall back to the file name's own decreasing version order) aren't
independent alternatives — rule 4's own text explicitly triggers "when
sort-key is not set **or those fields are not set or are all equal**",
meaning even when `sort-key` *is* set on both sides, if `sort-key`/
`machine-id`/`version` all end up comparing equal, the file-name
fallback still applies as a final tie-break. `compare_entries`
reflects that exactly: the `sort-key`-set-on-both branch chains all
four comparisons together (`sort-key`, then `machine-id`, then
`version` descending, then the file name descending) via `Ordering::
then_with`, rather than treating rule 2 as fully definitive on its
own — a real, non-obvious detail this note calls out specifically
because it would have been easy to implement the two rules as
separate, non-interacting branches instead and still look plausible
without a test catching the difference.

Rule 1 (bad entries sort last) is checked first and unconditionally,
before either rule 2/3/4 — a boot-counted entry with `tries_left`
still above zero is "indeterminate", not "bad", and correctly does
*not* get forced to the end just for having a boot-counting suffix at
all (a real distinction, tested directly: an indeterminate entry with a
suffix sorts by the ordinary rules, not last).

The real spec's own "Alphanumerical Order" rule for `sort-key`/
`machine-id` (`strcmp`-equivalent, absent/empty always sorts lower,
both absent/empty compares equal) needed no special-casing at all:
`Option::unwrap_or("")` plus a plain `str` comparison already gives
exactly that behavior, since an empty string is a prefix of, and
therefore already compares lower than, any non-empty one.

## Real, automated tests

6 new tests in `oci-bls::version`: every short worked example from the
real spec, the full ordered chain (checked pairwise in both
directions, not just adjacent pairs), reflexivity for every chain
entry, leading zeros not affecting magnitude, an absurdly long digit
run not panicking, both-empty comparing equal. 9 new tests in
`oci-bls::sort`: bad entries sort last regardless of file name;
an indeterminate (not bad) boot-counted entry is *not* treated as bad;
`sort-key` on only one entry sorts it earlier; `sort-key` on both
orders by `sort-key` first; equal `sort-key`s fall through to
`machine-id`; equal `sort-key`/`machine-id` fall through to decreasing
version; identical `sort-key`/`machine-id`/version fall through all
the way to the file name (the specific "rule 2 chains into rule 4"
behavior called out above); neither side has a `sort-key` falls back
to decreasing file-name version order directly; the boot-counting
suffix is stripped before comparing file names (without stripping it
first, the comparison would be wrong).

## Performance

This increment touches only the still-growing `oci-bls` crate —
`oci-runtime-core`/`ocirun`/`ociman run`'s own hot paths are untouched
(confirmed via `git diff --stat`), and `oci-bls` still isn't linked
into any binary's own hot path yet either, so no benchmark
re-verification was needed. No new external dependency (plain `str`
parsing and `slice::sort_by`, no regex or bignum crate).

## What's still not here

* Wiring `scan_entries` + `sort_entries` together into an actual
  `ociboot`-facing "list the boot menu in order" function — this
  increment gives the crate its own real, verified sorting primitive
  to build that on, not the higher-level feature itself.
* Atomic default-entry flips, the `boot_success`/
  `boot_indeterminate_count` grubenv protocol, kernel argument editing
  — all still exactly as 0070 left them.
