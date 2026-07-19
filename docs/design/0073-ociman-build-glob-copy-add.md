# Design note 0073: `ociman build` glob patterns in `COPY`/`ADD` (milestone 4)

Status: implemented
Scope: `crates/oci-dockerfile/src/glob.rs` (new — `contains_wildcards`/
`match_pattern`/`BadPattern`), `crates/oci-dockerfile/src/lib.rs`,
`bin/ociman/src/build.rs` (`resolve_sources`/`expand_wildcard_source`/
`walk_relative_paths`, both `copy_instruction`/`add_instruction` now
call `resolve_sources` instead of rejecting any source containing
`*`/`?`/`[`), `tests/tests/ociman_build.rs`.

Both `copy_instruction` and `add_instruction` have said "wildcard
patterns are not yet supported" since 0052/0068 respectively — the
last remaining item on both instructions' own explicitly-tracked scope
list (0072 already closed "multiple explicit sources").

## The matcher: a direct translation of Go's own `path/filepath.Match`, not a hand-rolled algorithm

Real BuildKit's own `copyWithWildcards`
(`~/git/moby/daemon/builder/dockerfile/copy.go`) calls Go's standard
library `filepath.Match` directly — so matching real `docker build`/
`podman build`'s own glob behavior means matching *that* algorithm
specifically, not shell globbing, not `regex`, not any other
plausible-looking pattern syntax. `/usr/share/go-1.22/src/path/
filepath/match.go` (the real, exact source this development host's own
Go toolchain ships) was read in full and translated function-for-
function: `scanChunk`, `matchChunk`, `getEsc`, and `Match`'s own
labeled-loop structure all have a direct Rust counterpart of the same
name and shape in `oci-dockerfile::glob`.

## Verified two independent, exhaustive ways before trusting it

1. **Two dozen hand-picked probe patterns** (covering `*`, `?`,
   `[...]`, `[^...]` negation, `\`-escaping, and the rule that none of
   those ever cross a `/`) were run through a real `go run` program
   against the real `go1.22.2` toolchain installed on this development
   host *before* a single line of the Rust translation was written,
   confirming the exact expected result for each.
2. **Go's own official, complete `matchTests` table**
   (`/usr/share/go-1.22/src/path/filepath/match_test.go`, 55 cases —
   the same table Go's own maintainers test their real implementation
   against) was copied verbatim into this crate's own test suite and
   run against the Rust translation directly. All 55 cases pass,
   including several that specifically probe multi-byte UTF-8 handling
   (`"a?b"` against `"a\u{263a}b"`, ranges over Greek letters) and a
   whole family of `ErrBadPattern` cases this note's next section
   covers.

## A real, easy-to-miss validation rule caught only by the official test table

`get_esc` (translating Go's own `getEsc`) has a final check that's easy
to miss on a first pass: after consuming one (possibly `\`-escaped)
character for a class member or range endpoint, *at least one more
byte must remain* in the pattern — there has to be room left for
either a range's own `-hi` or the class's closing `]`. Confirmed
directly: real `filepath.Match("[a", "a")` itself fails with
`ErrBadPattern`, not merely "no match" — a single valid character
followed immediately by end-of-string is still malformed, since a `[`
was opened but never properly closed. An earlier draft of this
translation missed this exact check; it was caught by Go's own
official test table (`{"a[", "a", false, ErrBadPattern}` among several
similar cases), not invented independently — a clear example of why
copying the *real* authoritative test data in verbatim, rather than
writing tests from a summary of expected behavior, matters.

## A real potential panic, found and fixed before it ever shipped

Go's own `matchChunk` advances one **byte** at a time for a literal
(non-`?`/non-`[...]`) pattern character (`chunk = chunk[1:]`, `s =
s[1:]`) — Go's byte-indexed strings tolerate landing mid-UTF-8-
character trivially. An earlier draft of this translation operated on
Rust `&str` directly and advanced one whole *character* at a time
instead, which looked equivalent for the ordinary case but would have
**panicked** on Rust's own char-boundary requirement the moment a
literal multi-byte character in the pattern was compared byte-by-byte
against name content that didn't line up on a boundary the same way.
Caught during translation (not by a failing test — the panic risk was
recognized directly from re-reading Go's own byte-slicing operations
side by side with the draft Rust code) and fixed by reworking the
entire module to operate on `&[u8]` internally throughout, only ever
decoding a full `char` at the exact points Go's own algorithm does
(`?`/`[...]`), with a `decode_char` helper (`a_multi_byte_utf8_name_
never_panics_regardless_of_pattern_shape`, a small fuzz-style test over
several multi-byte names and pattern shapes) confirming the fix.

## Wiring into `ociman build`: `resolve_sources`, shared between `COPY` and `ADD`

`resolve_sources` walks `source_root`'s entire tree (`walk_relative_
paths`, recursive, every entry at any depth, files and directories
alike — matching real BuildKit's own `filepath.WalkDir`, which the
real source uses for exactly this), computes each entry's own path
relative to `source_root`, sorts the results (matching `WalkDir`'s own
documented lexical-order guarantee), and keeps whichever ones
`match_pattern` accepts. A literal (non-wildcard) source passes
straight through unchanged, exactly as before this increment. The
existing "more than one source needs a trailing `/` on the
destination" rule (0072) is now checked against the *expanded* source
count, not the number of source arguments as literally written —
confirmed directly against the real source, whose own equivalent check
(`len(infos) > 1`) operates on the post-glob-expansion list too, so a
single glob pattern that itself expands to more than one real file
needs the same trailing `/`.

## Real, manual end-to-end verification before writing a single automated test

Built the debug binary and ran real builds by hand: `COPY *.txt /app/`
against a context with `a.txt`/`b.txt`/`c.md`/`subdir/nested.txt`
correctly copied only the two top-level `.txt` files (confirmed `c.md`
and the nested file were both genuinely absent from the result, not
just that the two expected ones were present); a pattern matching zero
files (`COPY *.nonexistent /dest/`) failed with the real "matched no
files" error; a pattern reaching into a subdirectory (`COPY
subdir/*.txt /app.txt`) correctly matched the nested file specifically;
the same `*.txt` pattern worked identically for `ADD`.

## Real, automated tests

4 new tests in `oci-dockerfile::glob` (the hand-verified probe set; the
full 55-case official Go test table; the multi-byte panic-safety
check; `contains_wildcards`'s own escaping rule). 3 tests in `tests/
tests/ociman_build.rs` updated or added: `copy_rejects_unsupported_
flags_and_bad_glob_patterns` (renamed, its own stale "wildcard rejected"
case replaced with a genuinely-still-rejected malformed pattern and a
genuinely-still-rejected zero-match pattern), plus two new positive
tests (`copy_expands_a_glob_pattern_against_the_build_context`,
checking both what's present *and* what's correctly absent;
`copy_expands_a_glob_pattern_that_reaches_into_a_subdirectory`).

## Performance

This increment touches only the still-growing `oci-dockerfile::glob`
module (pure, no I/O, not called from any other crate yet) and
`bin/ociman/src/build.rs`'s own `COPY`/`ADD` instruction handling — not
`oci-runtime-core`, `main.rs`'s `synthesize_spec`/`resources_from_cli`,
or either cgroup driver (confirmed via `git diff --stat`), and none of
this is on the `ociman run`/`ocirun run` startup/destroy hot path this
project's own benchmarks measure. No benchmark re-verification was
needed, consistent with every prior build-only increment.

## What's still not here

* `ADD`'s own remote URL sources, `COPY --from=<external-image>`, the
  build cache — all still exactly as before. `COPY`/`ADD` now share
  essentially the same real-Dockerfile-source-resolution feature set
  real `docker build`/`podman build` both provide, aside from those
  three.
