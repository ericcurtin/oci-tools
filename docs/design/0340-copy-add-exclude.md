# Design note 0340: `COPY`/`ADD --exclude=<pattern>`

Status: implemented
Scope: `crates/oci-dockerfile/src/{instruction,dockerignore,expand_stage,lib}.rs`,
`bin/ociman/src/build.rs`, `tests/tests/ociman_build.rs`.

## What this closes

Real BuildKit's own `COPY`/`ADD --exclude=<pattern>` (further narrowing
one instruction's own copied files on top of, or with no,
`.dockerignore`) was one of the handful of BuildKit-only flags this
project's own `oci-dockerfile` crate explicitly listed as "deliberately
not implemented yet." A dedicated scoping pass (flagged as needed by
`0337`'s own survey, since `COPY`/`ADD --link` — genuinely bigger, a
real build-executor architecture change — was ruled out from the same
survey) found this one to be a real, bounded, small-to-medium feature:
the exact pattern-matching engine already exists (`DockerIgnore`, built
for `.dockerignore`), it just needed a way to combine two independently
-sourced pattern lists correctly.

## Real, checked-directly semantics

Read real BuildKit's own source directly
(`~/git/moby/vendor/github.com/moby/buildkit/frontend/dockerfile/
instructions/parse.go`): `--exclude=` is `req.flags.AddStrings
("exclude")` — a real, repeatable string-list flag (`ExcludePatterns
[]string`), not a single value, on both `COPY` and `ADD`. Traced its
own consumer (`~/git/moby/vendor/github.com/tonistiigi/fsutil/
filter.go`): `patternmatcher.New(opt.ExcludePatterns)` — the *exact
same* gitignore-style pattern engine `.dockerignore` itself uses
(`docker/docker/pkg/patternmatcher`), confirming `DockerIgnore` (this
project's own already-built, already-tested `.dockerignore` compiler)
is the correct, already-proven primitive to reuse here too — no new
pattern-matching engine needed at all.

Crucially, `--exclude=` is a property of the `COPY`/`ADD` instruction
*itself*, not of the build-context-transfer step `.dockerignore`
belongs to — so, unlike `.dockerignore` (which this project's own
`copy_instruction` already deliberately never applies to a `COPY
--from=<stage>`/`--from=<external-image>` source), `--exclude=` must
apply *regardless* of `--from=` too.

## Combining two independently-sourced pattern lists correctly

The one real design question this scoping pass had to resolve: how do
`.dockerignore`'s own patterns and a single instruction's own
`--exclude=` patterns combine, when both apply to the same copy?

Simply checking both matchers independently and OR-ing the two
booleans together seems tempting, but can't correctly reproduce
`!`-negation that spans both sources (e.g. an instruction-level
`--exclude=!keep.txt` meaning "re-include a path `.dockerignore` itself
excluded" — negation order is exactly what makes a real, single
`patternmatcher` matcher's own semantics well-defined, and two
independently-evaluated matchers have no way to represent "this
pattern, from list B, should override that one, from list A"). The
correct fix: concatenate both **raw** pattern lists (in order —
`.dockerignore`'s own patterns first, this instruction's own
`--exclude=` values appended after) and compile **one** combined
matcher, exactly the same way a single `.dockerignore` file with more
lines would already behave.

This needed `DockerIgnore` to retain its own original raw pattern
strings (previously only kept the already-parsed, un-reconstructable
`CompiledPattern`s) — a new `raw` field plus a `raw_patterns()`
accessor, populated by `compile` and preserved through `empty()` too.

## Implementation

`CopyFlags`/`AddFlags` gained `exclude: Vec<String>`, parsed as a
repeatable flag in `parse_copy`/`parse_add` (matching real BuildKit's
own `AddStrings` shape) and threaded through `expand_stage`'s own
`$ARG`/`$ENV` substitution pass the same way `sources` already is.

New shared `combine_ignore_with_exclude(context_ignore, exclude) ->
anyhow::Result<Option<DockerIgnore>>` in `bin/ociman/src/build.rs`:
when `exclude` is empty (the overwhelmingly common case), simply
clones `context_ignore` unchanged — no recompilation cost at all for
every `COPY`/`ADD` that doesn't use this new flag. Otherwise
concatenates `context_ignore`'s own raw patterns (if any) with
`exclude` and compiles once. Wired into both `copy_instruction`
(shadowing its own `context_ignore` binding — every downstream call,
`resolve_sources`/`ensure_sources_exist`/`content_digest`/
`copy_path_recursive`, needed zero changes at all) and `add_instruction`
(shadowing its own `dockerignore` parameter, which — unlike `COPY`'s
`context_ignore` — is never `Option`, since `ADD` has no `--from` to
ever disable it for).

The content digest (`build_cache::content_digest`) already only hashes
what will actually be copied, so it automatically, correctly reflects
`--exclude=`'s own effect on the build cache with zero extra work
(a change to `--exclude=` that changes what's copied invalidates the
cache; one that doesn't, doesn't) — matching real podman's/buildah's
own established convention (`0130`-`0133`) of the cache never
double-counting an excluded path. `--exclude=` is deliberately not
added to the reconstructed `created_by` text `copy_add_command_text`
builds, matching the same precedent already established for
`--checksum` (also omitted there) — the content digest is what
actually drives cache correctness, not the literal flag text.

## Verified

`cargo test -p oci-dockerfile --locked`: 154 tests (152 pre-existing +
2 new parsing tests for `--exclude=` repeatability on both `COPY`/`ADD`,
plus 2 new `DockerIgnore::raw_patterns` unit tests) all pass.

Five new integration tests in `tests/tests/ociman_build.rs` (124 total,
119 pre-existing, all pass unchanged): a bare `--exclude=` with no
`.dockerignore` at all excludes the named pattern; `--exclude=` is
repeatable (two separate flags, both apply); `--exclude=` combines
correctly with an already-present `.dockerignore` (each excludes its
own, different file, both effects present); `--exclude=` applies even
to a `COPY --from=<stage>` source (unlike `.dockerignore`, which the
existing `dockerignore_does_not_apply_to_copy_from_an_earlier_stage`
test already confirms doesn't); and `ADD --exclude=` excludes a local
source the same way `COPY --exclude=` does.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (113 test result blocks,
0 failures), `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).

Performance: the no-`--exclude=` case (the overwhelmingly common one)
costs nothing beyond the new `Vec::is_empty()` check and an owned
`DockerIgnore` clone (a cheap `Vec<CompiledPattern>` copy, not a
recompile) — not part of any tracked benchmark in
`docs/benchmarks.md`. No re-benchmark needed.

## Still ahead

`COPY`/`ADD --link` remains confirmed genuinely bigger, not a small
follow-on: its real contract (an independently-cacheable, isolated
layer computed without reference to the accumulated rootfs state)
only makes sense in BuildKit's own LLB-graph execution model, and
would need this project's own build executor's whole-rootfs-diff
approach restructured into an isolated-layer-then-merge one — a real
architectural change. `COPY --parents`, `ADD --link`/`--keep-git-dir`/
`--unpack`, `RUN --mount=`, and heredocs remain separately-scoped,
each-bigger future candidates, as do `ociman`/`ocirun`'s other
remaining gaps (`--restart` policy, `--console-socket`) and `ocibox`'s
own remaining gaps (`stop`/`upgrade`/`generate-entry`/`assemble`,
`export --sudo`/`--enter-flags`).
