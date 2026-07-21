# Design note 0123: `ociman rmi` resolves by image ID too

Status: implemented
Scope: `bin/ociman/src/main.rs` (`ResolvedImage` new,
`resolve_image_by_reference_or_id`'s own return type changed,
`cmd_rmi`, `RmiResult`, `cmd_inspect` updated for the new return type);
`tests/tests/ociman_rmi.rs` (3 new tests).

## Closing the gap 0122 deliberately left open

0122 added image-ID resolution to `ociman inspect` only, explicitly
noting `rmi`/`tag` needed "real extra design care for the 'multiple
tags share this ID' removal-ambiguity case" and left them for a future
increment. This increment does that design work and implements it for
`rmi` (the one of the two where the ambiguity actually matters — `tag`
only ever *adds* a pointer, never removes one, so there's no analogous
"which tag do I affect" question for it).

## Real policy, checked directly against a real `podman rmi`, not assumed

Before writing any code: a real local store, one image with two tags
(`docker.io/library/busybox:latest` and a second `ociman tag`-style
alias), then `podman rmi <the-shared-id>`:

* **Refuses without `--force`**: `Error: unable to delete image
  "<full-digest>" by ID with more than one tag ([localhost/myalias:latest
  docker.io/library/busybox:latest]): please force removal` — neither
  tag touched.
* **`podman rmi -f <id>`**: untags both, deletes the image.
* **A single-tagged image removed by ID**: works with no `--force`
  needed at all (no ambiguity — exactly one tag to remove).
* **Removing by an exact *tag*** (not an ID), even with a sibling tag
  present: always just untags that one name, no `--force` needed,
  sibling left completely untouched.

`ResolvedImage` (new: `Tag(ImageRecord)` / `Id(ImageRecord)`) is what
lets `cmd_rmi` tell these cases apart — `resolve_image_by_reference_
or_id`'s own return type changed from `Option<ImageRecord>` to
`Option<ResolvedImage>` to carry that distinction through (`cmd_inspect`,
the only other caller, doesn't care which arm it got and just calls
`.record()`).

## Implementation

`cmd_rmi` resolves `reference_str` once via `resolve_image_by_
reference_or_id`. `ResolvedImage::Tag` behaves exactly as before (one
reference to remove, no multi-tag policy question). `ResolvedImage::Id`
collects every tag sharing that exact manifest digest
(`store.list_images()` filtered by `manifest_digest ==`, sorted for a
deterministic error message) and, unless `--force` or there's only one,
refuses with real `podman`'s own message shape (adapted to this
project's own established error-message style). Both the dependent-
container check and the actual removal loop now operate over the
*whole* set of references being removed (one, in the tag case; possibly
several, in the forced-ID-with-siblings case) instead of a single
`canonical` string.

`RmiResult`'s own JSON shape gained one new, always-optional field
(`additional_references_removed`, `skip_serializing_if = "Vec::is_
empty"`) rather than replacing `reference: String` with a `Vec` outright
— the existing `rmi_json_reports_the_canonical_reference_and_any_
removed_containers` test (asserting `view["reference"]` as a plain
string) needed no changes at all, since the common, single-tag-removal
case's own JSON shape is completely unchanged.

## Real, automated tests

Three new tests: a single-tagged image removed by its own short ID,
no `--force` needed; two tags sharing an image, `rmi <id>` refusing
with the real error wording (both tags left untouched), then `rmi -f
<id>` removing both; and an exact-tag removal never needing `--force`
just because a sibling tag exists (the sibling surviving). All 6
pre-existing `ociman rmi` tests and all 10 pre-existing `ociman prune`
tests (which also exercise `rmi`) still pass unmodified. Full `cargo
build --workspace --locked`/`cargo test --workspace --locked` (2 clean
runs)/`cargo fmt --all --check`/`cargo clippy --workspace --all-targets
--locked -- -D warnings` all clean.

## What this doesn't do yet

* `ociman tag`'s own source argument still only resolves by tag
  reference, not by ID — `ociman tag <id> <new-tag>` isn't supported
  yet. Lower priority than `rmi` (tagging an image you can already see
  the full reference for in `ociman images` is a smaller usability gap
  than not being able to remove one by the ID column that same command
  prints), left for a future increment if it turns out to matter in
  practice.
