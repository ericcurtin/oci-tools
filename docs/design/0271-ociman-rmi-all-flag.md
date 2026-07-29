# Design note 0271: `ociman rmi -a`/`--all`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_rmi.rs`.

## Closing the last real gap from `0269`/`0270`

`0269`/`0270` closed real `podman rmi`'s multi-reference and
`--ignore` gaps but deliberately deferred `-a/--all` pending a
dedicated design for a real edge case: a manifest digest with both
several real tags *and* an untagged sentinel record (`0179`) present
at once. This note closes that gap.

## The edge case, and why it turned out to be a non-issue

The concern: if `--all` simply looped over every stored record and
called the existing `rmi_one` with each record's own reference string
re-resolved through the ordinary tag-then-ID-fallback machinery, the
untagged sentinel's own reference string (a bare `sha256:<hex>`, no
`/` at all) doesn't parse as a real tag reference at all and would
fall through to *ID* resolution — triggering the by-ID sibling-tag-
ambiguity gate (`rmi <id>` needs `--force` when more than one tag
shares that digest) depending on which order the records happened to
be processed in, since removing earlier sibling tags first would
shrink the "how many tags share this digest" count out from under a
later attempt on the same digest.

The fix is simpler than the concern: `--all` already has every record
in hand from `store.list_images()` — there's no ambiguity to resolve
at all, since each one is already known exactly. Wrapping each
snapshotted record directly as `ResolvedImage::Tag` (never re-
resolving it as a fresh spec string) bypasses the by-ID ambiguity path
entirely, matching this project's own established one-row-per-
reference data model: `--all` removes each already-enumerated pointer
record independently, one at a time, exactly like the plain multi-
reference case (`0269`) already does for a list of tag references —
`rmi_one` itself needed zero changes.

Verified by hand with the exact scenario the concern described: a
digest with two real tags plus an untagged sentinel, all sharing one
manifest digest — every one of the three records removed cleanly in
one `--all` call, no ambiguity error at all, confirmed order-
independent (also covered by a new automated test).

## Semantics, checked directly against a real installed `podman`

- `--all` removes every image in local storage — not just dangling
  ones, unlike `ociman prune`'s own default (checked directly: real
  `podman rmi --all` removes a still-tagged, in-use-by-nothing image
  too, not only untagged ones).
- Still refuses an image any container depends on unless `--force` is
  *also* given (checked directly: a real `podman rmi --all` alone,
  without `--force`, left a container's own image untouched while
  removing an unrelated, unused one in the same call).
- Every other image is still attempted even if one fails partway
  through, matching real `podman rmi`'s own multi-target behavior and
  this project's own `ociman rm --all`/`ocibox rm --all`'s identical
  policy.
- `--all` and an explicit reference together is a clear error, never
  an ambiguous silent choice between the two.
- A real, silent no-op on an already-empty store, matching this
  project's own established convention.
- `--json rmi --all` always prints a JSON array (one entry per image
  actually removed), never the single-object shape reserved for
  exactly one explicit reference (`0269`).

## Verified

Integration (`tests/tests/ociman_rmi.rs`, four new tests):

- `--all` removes every image from a two-image store, printing each
  removed reference; a second `--all` call on the now-empty store is
  a real, silent no-op.
- `--all` combined with an explicit reference is a clear, immediate
  error.
- A mix of one free image and one in-use image: `--all` without
  `--force` removes the free one, leaves the in-use one untouched,
  and still surfaces the one real failure; `--all --force` then
  removes everything.
- The exact edge case above (two real tags plus an untagged sentinel
  sharing one digest): all three records removed in one `--all` call
  with no ambiguity error.

Regression: all 17 pre-existing `ociman_rmi.rs` tests still pass
unmodified.

Full workspace: `cargo build`/`test --workspace`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`python3 ci/guards.py`, `cargo deny check`, `ci/native-ci.sh`,
`ci/build-deb.sh` (one known, pre-existing flake in
`ocicri_container.rs`'s `exec_sync_runs_commands_in_a_running_container`
was hit twice under full-parallel load during this verification,
unrelated to this change — confirmed passing reliably in isolation
and on a clean re-run of the full suite).

## Still ahead

Real `podman rmi`'s own flag set is now fully matched
(`-a/--all`, `-i/--ignore`, `-f/--force`; `--no-prune` found in `0270`
not to apply to this project's own content-addressed store at all).
</content>
</invoke>
