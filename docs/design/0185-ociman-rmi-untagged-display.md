# Design note 0185: `ociman rmi`'s untagged-sibling display

Status: implemented
Scope: `bin/ociman/src/main.rs` (`RmiResult`'s `reference`/
`additional_references_removed` fields, new `display_reference`
helper, `cmd_rmi`'s error message and output); `tests/tests/
ociman_rmi.rs`.

## Closing a gap flagged in 0179, reconfirmed still open in a
subsequent survey

0179's own "what this doesn't do yet" section named this directly:
`cmd_rmi`'s sibling-list display shows the raw internal untagged-image
sentinel string (`untagged_reference`, a bare `sha256:<hex>`) verbatim
if it ever shares a digest with a real tag, instead of the `<none>`
placeholder `ImageView`/`BuildResult`/`CommitResult` already show for
the exact same situation elsewhere. A later survey (ahead of 0182)
re-confirmed this was still open. This increment closes it — a small,
low-risk, purely cosmetic fix (removal by ID already worked correctly
either way; only the *display* was wrong).

## A real, reachable case, not hypothetical

Traced through exactly how this can happen: `ociman build`/`ociman
commit` with no tag record the built/committed image under the
sentinel (0179/0180); a later `ociman tag <that-id> some/real:tag`
then adds a second, real-tag record pointing at the *same* manifest
digest (0179's own real-tag-wins tie-break in `resolve_image_by_id_
only` already handles *resolving* this correctly — it's specifically
`cmd_rmi`'s own separate `siblings` listing, which intentionally
enumerates *every* record sharing that digest rather than picking
just one, that never applied the same untagged-aware display before
this fix).

## The fix: keep the real reference for removal, only translate it for
display

`references_to_remove` (built from `siblings`) has to keep the real,
raw reference string for the actual removal step
(`store.remove_image(reference)` needs the exact key `put_image`
stored it under, sentinel included) — so the fix is purely additive: a
small `display_reference` helper (`<none>` for
`is_untagged_reference`, the string itself otherwise) applied only at
the three places this ever reaches a human or a machine-readable
output:

* The "unable to delete image ... by ID with more than one tag (...)"
  refusal message.
* The plain-text output listing every reference actually removed.
* `--json`'s own `RmiResult.reference`/`additional_references_removed`
  — both now `Option<String>`/`Vec<Option<String>>`, `None`/`null` for
  the sentinel, matching the exact same convention `ImageView`/
  `BuildResult`/`CommitResult` already established for their own
  identically-shaped "might be the sentinel" reference fields.

## Tests

New integration test: seeds a real tagged image, adds a second,
untagged pointer at the exact same digest directly via `Store::
put_image` (the same sentinel shape `ociman build`/`commit` produce),
then confirms `ociman rmi <id>` (no `--force`, multiple siblings)
shows `<none>` — never the raw sentinel string — in the refusal
message, and that `ociman rmi --force --json <id>` afterward reports
the real tag as `reference` and `null` (not the raw sentinel) in
`additional_references_removed`, while still correctly removing both
pointers. All 9 pre-existing `ociman rmi` tests continue to pass
unchanged. Full `cargo build --workspace --locked`/`cargo test
--workspace --locked` (2 clean runs)/`cargo fmt --all --check`/`cargo
clippy --workspace --all-targets --locked -- -D warnings`/`python3
ci/guards.py`/`cargo deny check`/`bash ci/native-ci.sh` all clean.

## A quick, targeted performance sanity check first

Before starting this increment, `ociman build` (external-image,
single-`RUN`, `--no-cache`) was benchmarked directly against `podman
build` for the same Containerfile — the first time `ociman build`
itself (as opposed to `run`/`create`/`commit`) has been directly timed
against `podman build` since 0121's original ~26%-speedup measurement,
and specifically relevant given 0184 (the immediately preceding
commit) added a real, new per-file `HashMap` check to `clone_cache_
tree`, the exact code path an external-image build always exercises.
Result: 18.4ms vs 342.7ms, an 18.67× win — confirms 0184's own
hardlink-tracking fix costs nothing measurable, the same conclusion
0169 already reached for the analogous change in `oci_layer::
write_entry`. No regression; no dedicated re-verification document
needed for this alone, but recorded here since it directly motivated
checking before starting this turn's own work.

## What this doesn't do yet

Nothing new — this closes 0179's own last-named `rmi`-specific
display gap completely.
