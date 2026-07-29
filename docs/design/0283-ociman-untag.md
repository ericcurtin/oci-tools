# Design note 0283: `ociman untag`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_tag.rs`.

## A genuinely new, top-level, self-contained command

Comparing `ociman --help`'s full command list directly against a real
installed `podman --help` (rather than continuing the recent `ps
--filter` streak) turned up `untag` — a real top-level podman/docker
command with zero coverage here, and one that doesn't need any new
architecture the way `pod`/`network`/`search` do: this project's own
image store is already one-pointer-per-reference, and `untag` is
exactly "remove one of those pointers without touching the underlying
blobs" — precisely what `ociman tag`'s own sibling command already
implies is possible, just in reverse.

## Real semantics, checked directly against a real installed `podman`

`podman untag IMAGE [IMAGE...]` (checked directly, not just read from
source — this project's own recent history of installed-vs-cloned-
source mismatches made verifying this by hand worthwhile again):

- `IMAGE` (the first argument) *resolves* the target — by tag
  reference or a real/short image ID, same as `ociman tag`'s own
  `source` — but is **not itself untagged** unless it also appears
  among the following arguments (or unless no further arguments are
  given at all).
- With further arguments given: **only those specific references are
  removed**, one at a time. Verified directly: `podman untag alias1
  alias2` (a real image with three tags: the original name, `alias1`,
  `alias2`) removed *only* `alias2` — the resolving reference
  (`alias1`) and the original name both survived untouched.
- **With no further arguments** (`IMAGE` alone): *every* real
  reference/tag currently pointing at that image is removed instead —
  verified directly and initially surprising (a first assumption that
  only the one given reference would be removed was wrong): a single-
  argument `podman untag alias1` against that same three-tagged image
  removed all three at once, leaving the image itself completely
  untagged (still present in storage, `<none>:<none>` in `podman
  images`, exactly the same as this project's own untagged-image
  sentinel convention, `0179`).
- An explicit reference that doesn't currently point at the *same*
  image `IMAGE` resolved to is a clear error ("tag not known"),
  removing nothing else in the same call either — verified directly
  with a real, unrelated second image.
- Never touches the underlying blobs — a real, silent no-op for
  `ociman prune` to reclaim later, exactly like any other now-dangling
  image.
- No sibling-tag-ambiguity/`--force` gate at all, unlike `ociman rmi`'s
  by-ID case: removing a tag *pointer* is never destructive to
  anything depending on the image the way removing the image itself
  (`rmi`) would be, so there's no ambiguity risk to guard against.

## Implementation

`resolve_image_by_reference_or_id` (already shared by `rmi`/`tag`/
`inspect`) resolves `IMAGE`. Each explicit reference argument is
normalized via `Reference::parse` (matching `cmd_tag`'s own identical
pattern) and looked up directly via `store.resolve_image` (an exact
tag lookup, not falling back to ID resolution — matching real podman's
own literal "tag not known" error, which implies a name lookup, not a
broader ID search), then checked for `manifest_digest` equality with
the resolved `IMAGE` before being removed via `store.remove_image`. No
arguments at all falls back to every sibling of the same digest
(`store.list_images()` filtered by `manifest_digest`, the exact same
grouping `ociman rmi`'s own by-ID sibling collection already uses).

## Verified

Integration (`tests/tests/ociman_tag.rs`, five new tests):

- An explicit reference removes only that one; the resolving reference
  and the original source both survive.
- A single argument removes every tag of that image.
- An unrelated reference (belonging to a different image) is a clear
  error, removing nothing at all.
- An unresolvable `IMAGE` argument is a clear error.
- `IMAGE` resolves by a real or short image ID too, not just a tag.

Full workspace: `cargo build`/`test --workspace`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`python3 ci/guards.py`, `cargo deny check`, `ci/native-ci.sh`,
`ci/build-deb.sh` (one known, pre-existing flake in `ociman_logs.rs`'s
`logs_follow_streams_a_running_containers_output_and_stops_when_it_exits`
was hit once under full-parallel load, unrelated to this change —
confirmed passing reliably in isolation and on a clean re-run of the
full suite).
</content>
</invoke>
