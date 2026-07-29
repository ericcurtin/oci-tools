# Design note 0270: `ociman rmi -i`/`--ignore`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_rmi.rs`.

## Closing another real gap noted in `0269`

`0269`'s own "still ahead" note listed real `podman rmi`'s remaining
flags, checked directly against a real installed `podman rmi --help`:
`-a/--all`, `-i/--ignore`, `--no-prune`. This note closes `-i/--ignore`
— a reference that doesn't resolve to any real image is a silent
no-op instead of a clear error, matching real `podman rmi --ignore`
exactly.

`-a/--all` and `--no-prune` are deliberately left for later,
separately-scoped work: `--no-prune` real semantics ("don't also
remove a now-dangling *parent* image after removing a child") don't
actually apply to this project's own content-addressed store at all —
checked directly by building a real two-`RUN`-step image and
confirming `ociman images` only ever lists the *one* final tagged
result, never a separate "image" per intermediate build layer the way
real podman's own graph-driver storage model does, so there's nothing
real for `--no-prune` to opt out of here yet. `-a/--all` was
considered but deliberately deferred after research surfaced a real
edge-case interaction between it and the existing sibling-tag-ambiguity
gate (a manifest digest with both several real tags *and* an untagged
sentinel record) whose correct, order-independent behavior needs its
own dedicated design rather than folding into this smaller, safer
increment; noted below as still ahead.

## Semantics, checked directly against a real installed `podman`

- `--ignore`/`-i`: a reference that doesn't resolve to any real image
  at all is a silent, successful no-op — no error, no output.
- `--force` implies `--ignore` too, checked directly (`podman rmi
  --force some-bogus-name` exits `0` with no output, exactly like
  `podman rmi --ignore some-bogus-name`) — even though `--force` alone
  says nothing about resolution failures in its own help text.
- `--ignore` only ever silences *that one* specific failure mode —
  checked directly: `podman rmi --ignore <image-in-use-by-a-container>`
  (no `--force`) still reports the in-use error exactly as before,
  confirmed with a real running dependent container.
- Combining `--ignore` with a mix of valid and nonexistent references
  removes every valid one and silently skips every nonexistent one,
  composing correctly with the multi-reference continue-past-failure
  policy `0269` already established.

## Implementation

`resolve_image_by_reference_or_id` is now called directly in
`cmd_rmi`'s own per-reference loop (rather than inside `rmi_one`),
letting the loop distinguish "doesn't resolve to anything at all"
(subject to `--ignore`) from every other failure kind (never subject
to it) without any error-message string-sniffing. `rmi_one` itself was
narrowed to take an already-resolved `ResolvedImage` instead of doing
its own resolution — a pure signature change with its actual removal
logic (sibling-tag gate, dependent-container gate, actual removal)
completely unchanged, confirmed by all 13 pre-existing tests passing
unmodified.

## Verified

Integration (`tests/tests/ociman_rmi.rs`, four new tests):

- `--ignore` on a lone nonexistent reference: silent success (no
  stdout, no stderr).
- `--force` alone (no `--ignore`) on a lone nonexistent reference:
  also silent success, matching the real, checked-directly implication
  above.
- `--ignore` on an image still in use by a container (no `--force`):
  still a clear, reported error, the image untouched.
- `--ignore` with one valid and one nonexistent reference together:
  the valid one is removed, the nonexistent one silently skipped, the
  overall call succeeds.

Regression: all 13 pre-existing `ociman_rmi.rs` tests still pass
unmodified after the `rmi_one`/`cmd_rmi` split.

Full workspace: `cargo build`/`test --workspace`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`python3 ci/guards.py`, `cargo deny check`, `ci/native-ci.sh`,
`ci/build-deb.sh`.

## Still ahead

`ociman rmi -a/--all` (remove every image, matching real `podman rmi
--all` — still refuses one a container depends on unless `--force`
too) remains a real candidate, deliberately deferred here pending a
dedicated design for its own correct, order-independent interaction
with the existing by-ID sibling-tag-ambiguity gate when a manifest
digest has both several real tags and an untagged sentinel record
present at once.
</content>
</invoke>
