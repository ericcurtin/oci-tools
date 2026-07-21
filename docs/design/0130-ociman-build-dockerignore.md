# Design note 0130: `.dockerignore` support for `ociman build`

Status: implemented
Scope: `crates/oci-dockerfile/src/dockerignore.rs` (new — `parse`,
`DockerIgnore::{compile,is_ignored,has_negation}`, `clean_path`);
`bin/ociman/src/build.rs` (`cmd_build` reads/compiles a build's own
`.dockerignore` once; `StageContext` gains a `dockerignore` field;
`resolve_sources`/`ensure_sources_exist`/`expand_wildcard_source`/
`walk_relative_paths`/`copy_path_recursive` all gain a
`.dockerignore`-aware parameter, threaded from `copy_instruction`/
`add_instruction`); `tests/tests/ociman_build.rs` (8 new tests).

## Why this, now

Real `.dockerignore` support was a real, user-visible gap in `ociman
build`'s own milestone-4 scope: every `COPY`/`ADD` source copied
*everything* under a matched directory unconditionally, with no way
to exclude a `.git`/`node_modules`/build-artifact directory the way
every real `docker build`/`podman build` user takes for granted. Not
flagged in any single earlier design doc's own "what this doesn't do
yet" section (unlike 0129's `--tls-verify` gap) — found by directly
comparing this project's own `ociman build` behavior against real
`podman build`'s documented feature set while looking for the next
well-scoped milestone-4 increment.

## Verified against a real `podman build` first, not assumed from docs

Before writing any Rust, a real, installed `podman build` (4.9.3) was
used to pin down several non-obvious rules purely from documentation
prose would have gotten wrong or left ambiguous — every one of these
is also called out directly in `oci_dockerfile::dockerignore`'s own
module doc comment, with the exact pattern/result:

* A bare pattern (`*.log`) only ever matches a **top-level** context
  entry — `subdir/nested.log` survives; `**/*.log` is needed to match
  at any depth.
* A later `!pattern` re-inclusion works for one specific file even
  when an earlier pattern excluded its own parent directory — unlike
  real `.gitignore`'s own early-pruning traversal.
* Neither the Containerfile nor `.dockerignore` itself gets any
  special always-included treatment.
* An explicitly-named (non-wildcard) `COPY`/`ADD` source that's
  excluded fails exactly like a genuinely missing one (real podman's
  own error: `"no items matching glob ... copied (1 filtered out
  ...): no such file or directory"`) — not a separate "excluded by
  .dockerignore" message.
* A wildcard source silently drops an excluded match (no error), as
  long as at least one non-excluded match remains.
* A trailing `subdir/**` pattern excludes everything *inside*
  `subdir` but leaves `subdir` itself (now empty) untouched — real
  BuildKit's own `compile()` takes a `prefixMatch` fast path for
  exactly this shape (checking for the pattern's own literal `subdir/`
  prefix, which by construction never equals `subdir` alone) rather
  than the "zero-or-more" semantics a middle-of-pattern `**` genuinely
  has.

## A hand-rolled matcher, not a regex dependency

Real BuildKit/classic-builder's own implementation
(`~/git/moby/vendor/github.com/moby/patternmatcher/{patternmatcher,
ignorefile/ignorefile}.go`, read directly) compiles each pattern into
a Go regular expression. This project has no `regex` crate dependency
at all (see the workspace `Cargo.toml`'s own comments — pure-Rust
alternatives are preferred throughout, e.g. 0116's hand-rolled Go-
duration parser, 0126's hand-rolled base64), so [`DockerIgnore`]
instead splits each pattern into `/`-separated segments and matches
them with a direct, recursive backtracking algorithm (segments handled
by this crate's own pre-existing [`glob::match_pattern`] — already a
byte-for-byte, independently-verified port of Go's own
`path/filepath.Match`, reused here unchanged for anything that isn't a
literal `**` segment) plus one dedicated case for a `**` segment
(zero-or-more path segments, crossing `/`, with the one narrower
"trailing `**` requires at least one more segment" exception above).

**Deliberately narrower than real BuildKit** in one specific,
documented way: a pattern segment *mixing* `**` with other characters
(`a**b`) falls back to `glob::match_pattern`'s own ordinary single-`*`
collapsing instead of real BuildKit's regex-based "any number of path
segments" semantics for that shape — judged a safe, narrow first
increment (every real `.dockerignore` this project's own authors could
find only ever uses `**` as a whole segment) rather than justifying a
real regex engine as a new dependency for an edge case never seen in
practice.

`clean_path` is a direct, from-scratch port of Go's own
`path/filepath.Clean` (Unix-only — this project only ever runs on
Linux), needed because real `.dockerignore` patterns (and the paths
they're matched against) are lexically normalized before comparison —
every probe case in its own test was first run through a real `go run`
program against the real `go1.22.2` toolchain installed on this
development host, confirming the expected output before being copied
into the Rust test table (same rigor `oci_dockerfile::glob`'s own test
suite already established for `filepath.Match`).

## Real, measurable perf care, not just correctness

`.dockerignore`'s own `Exclusions()`/[`DockerIgnore::has_negation`]
flag is used to prune a directory-tree walk entirely once it's known
nothing under an excluded directory could ever be re-included by a
later `!` pattern — both `walk_relative_paths` (wildcard `COPY`/`ADD`
source expansion) and `copy_path_recursive` (the actual file copy)
skip descending into (and, for the copy, doing any work for) an
excluded directory outright when there's no negation pattern anywhere
in the file. For the overwhelmingly common real-world case — excluding
a large `.git`/`node_modules`/build-artifact directory, with no `!`
pattern anywhere — this avoids walking (or copying) that subtree at
all, a real, measurable saving directly in service of this project's
own "beat the equivalent tool's benchmarks" goal, not just a
correctness box to check.

## Where it applies, and deliberately doesn't

`.dockerignore` is purely a build-*context* concept: it only ever
filters `COPY`/`ADD` sources read from the context directory itself
(`flags.from.is_none()` for `COPY`; always true for `ADD`, which has
no `--from` at all). `COPY --from=<stage>` and `COPY
--from=<external-image>` are completely unaffected — neither one is
"the build context" — confirmed both by reading real BuildKit's own
context-transfer-time integration point and with a dedicated
automated test (`dockerignore_does_not_apply_to_copy_from_an_earlier_
stage`). An `ADD` source that's a local archive still has its own
*contents* untouched by `.dockerignore` either way (only whether the
archive file itself is copied at all is ever affected) — `.dockerignore`
was never a real thing real docker applies *inside* an archive.

## Real, automated tests

19 new unit tests in `crates/oci-dockerfile/src/dockerignore.rs`
(pattern parsing, `clean_path` against the real Go-toolchain-verified
probe table, exact/bare-star/double-star/negation matching, `has_
negation`, bad-pattern rejection) plus 8 new CLI-level integration
tests in `tests/tests/ociman_build.rs`, each built around a real
`ociman build` + `ociman run` round trip (not just inspecting the
built manifest) so a wrongly-copied or wrongly-excluded file would
actually be caught reading it back out of a real running container:
excluding a named context file from a whole-context `COPY .`; a bare
`*.log` pattern only matching top-level, not nested; `**/*.log`
matching at any depth; `!`-negation re-including one file under an
excluded directory; `.dockerignore` applying to `ADD`'s own local
sources too; an explicit ignored `COPY` source failing with "does not
exist"; a wildcard `COPY` silently dropping an ignored match; and
`.dockerignore` never applying to `COPY --from=<stage>`. All pre-
existing tests (the full 53-test `ociman_build.rs` suite included)
still pass unmodified. Full `cargo build --workspace --locked`/`cargo
test --workspace --locked` (2 clean runs)/`cargo fmt --all --check`/
`cargo clippy --workspace --all-targets --locked -- -D warnings`/
`python3 ci/guards.py`/`cargo deny check` all clean.

## What this doesn't do yet

* The `a**b` mixed-segment gap above.
* No `.containerignore` fallback name (real podman/buildah also
  accept `.containerignore` as an alternate file name, preferring it
  over `.dockerignore` when both exist) — only `.dockerignore` itself,
  matching this increment's own narrower first-pass scope; a small,
  well-scoped follow-up if it turns out to matter in practice.
* `walk_relative_paths`'s own perf-pruning optimization doesn't extend
  to the *matched-but-still-excluded* case within an already-walked,
  negation-containing directory — every entry under such a directory
  is still individually re-checked, rather than the more sophisticated
  "reuse the parent's own already-computed per-pattern match state"
  optimization real BuildKit's own `MatchesUsingParentResults` uses.
  Judged an acceptable, narrower-scope trade-off (a real, correct, if
  not maximally-optimized, first increment) rather than a correctness
  gap — the common, perf-critical case (no negation at all) is already
  the fully-optimized, prune-eagerly path.
