# Design note 0179: untagged `ociman build`

Status: implemented
Scope: `bin/ociman/src/main.rs` (new `untagged_reference`/
`is_untagged_reference`, `cmd_push`'s new guard, `ImageView`'s
`reference` becoming `Option<String>`, `resolve_image_by_reference_or_id`'s
tie-breaking fix); `bin/ociman/src/build.rs` (`cmd_build`'s `tag`
handling, `BuildResult`); `bin/ociman/src/archive.rs` (`save_oci_archive`/
`save_docker_archive` omitting a tag for an untagged image, both load
paths now recording one under the sentinel instead of no pointer at
all); `tests/tests/ociman_build.rs`, `tests/tests/ociman_push.rs`,
`tests/tests/ociman_load.rs`.

## Closing a real, explicitly-named deferred gap

`cmd_build`'s own doc comment used to say outright: "`-t`/`--tag` is
required... this project's `oci_store::Store` has no 'anonymous image,
addressable only by ID' concept yet." This increment closes it —
scoped to `ociman build` only; `ociman commit`'s own identical,
already-documented gap (`Command::Commit`'s `image` argument) is a
separate, natural companion change left for later.

## The store needs no schema change at all

Investigated directly before writing any code: `oci_store::ImageRecord`
is `{ reference: String, manifest_digest: Digest }`, and nothing in
`Store::put_image`/`resolve_image`/`list_images` ever validates
`reference` against `Reference::parse` — it's just an opaque key. So
rather than invent a second, ID-keyed record type (real podman's own
`containers/storage` model, `ID`-primary with a separate `Names []string`
list — a fundamentally different shape this project doesn't have and
didn't need to adopt), the minimal move is a **purely additive
convention**: record an untagged image under a synthetic sentinel
reference — the image's own manifest digest, verbatim (`sha256:<hex>`,
[`untagged_reference`]).

This sentinel can never collide with a real tag: every real,
`Reference::parse`-derived reference's own `Display` impl always
writes `<registry>/<repository>...` (checked directly against
`Reference`'s own `Display` in `crates/oci-spec-types/src/reference.rs`)
— always at least one `/`. A bare digest string never has one.
[`is_untagged_reference`] is exactly `!reference.contains('/')`.

## `resolve_image_by_reference_or_id` already worked for free

0122's own ID-resolution fallback (`resolve_image_by_reference_or_id`)
matches purely on `manifest_digest`, never inspecting `.reference`'s
own shape at all — so `ociman inspect`/`rmi`/`tag`/`push`/`save` could
all already resolve an untagged record by ID with **zero** changes.
One real, checked-directly fix was still needed there: when the exact
same digest has *both* a real tag and the untagged sentinel (e.g. an
untagged build later given a real tag via `ociman tag`), the existing
`HashMap::entry().or_insert()` dedup logic kept *whichever* record
`list_images()` happened to visit first — non-deterministic, and could
make `cmd_push`'s own new guard (below) misfire against an image that
in fact has a perfectly good real tag to push to. Fixed by preferring
a real reference over the sentinel whenever both exist for the same
digest, deterministically, regardless of iteration order.

## A real, previously-latent bug this increment's own investigation
surfaced and fixed: `cmd_push`'s silent bad-parse

`cmd_push` builds its own push destination via
`Reference::parse(&record.reference)` unconditionally. Traced through
`Reference::parse`'s own logic directly: fed the sentinel
(`"sha256:<hex>"`, no `/` at all), it does **not** error — it
misparses as repository `sha256`, tag `<hex>`, registry `docker.io`
(`split_domain` sees no `/` at all, so there's no domain component;
the `:` splits `repo_path`/`tag` as usual). `ociman push` has no
separate `DESTINATION` argument at all (0127), so an untagged image
genuinely has no reference to push to — this is now a real, clear,
immediate error (`"cannot push an untagged image"`) checked *before*
ever reaching that misparse, not a silent attempt to push to a
nonsense destination.

## `save`/`load`: symmetric, and a second real gap closed along the way

`save_oci_archive`'s `org.opencontainers.image.ref.name` annotation and
`save_docker_archive`'s `RepoTags` both used to write `record.reference`
verbatim — same landmine as `cmd_push`, just on the write side of an
archive instead. Both now omit the tag entirely for an untagged image
(an empty `RepoTags`/no annotation at all), matching each format's own
already-established, real convention for an untagged/by-digest image.

The *load* side had its own, different, pre-existing gap, found while
auditing every place `record.reference`/an untagged record gets
handled: `load_archive`'s own doc comment already said, correctly,
that loading an archive with no real tag annotation "is a real,
supported case, not an error" — but the actual code, on that path,
skipped calling `put_image` entirely. That means a loaded untagged
image had **no pointer at all**: invisible to `ociman images`,
unresolvable by ID (`resolve_image_by_reference_or_id`'s fallback only
ever scans `list_images()`), and would be silently swept by the very
next `ociman prune`. This was a real, if narrow, latent bug — not
hypothetical: it directly contradicted the doc comment's own stated
intent. Both load paths (`oci-archive`'s index-annotation-based one and
`docker-archive`'s `RepoTags`-based one) now record the loaded image
under the untagged sentinel instead, so it's fully visible/resolvable/
GC-safe — while `LoadedImage.references` itself still correctly
reports empty, preserving today's "Loaded image: ..." output and this
module's own already-correct doc comment, unchanged.

## Display: `<none>`, never the internal sentinel string

`ImageView.reference` (the type `ociman images --json`/`ociman pull
--json` serialize) is now `Option<String>` — `None` for an untagged
image, never the raw sentinel. The plain-text table shows real
`docker images`/`podman images`'s own `<none>` placeholder for that
column, matching this project's own single, narrower `REFERENCE`
column (rather than podman/docker's separate `REPOSITORY`/`TAG`
columns) exactly the same way.

## Output for an untagged build

Matches real `docker build`/`podman build` with no `-t`: just the
digest, no "tagged: ..." line at all (there's no tag to report).
`--json`'s own `BuildResult.reference` is `Option<String>`, `null` for
an untagged build.

## Tests

New unit tests (`main.rs`): `untagged_reference`/`is_untagged_reference`
round-trip, and that every real `Reference::parse`-derived reference
(several shapes) is never mistaken for the sentinel. Updated one
existing test (`requires_a_tag`, now `build_with_no_tag_at_all_still_
succeeds_and_records_an_untagged_image`) that previously asserted the
now-removed hard error; it now proves the opposite end-to-end: a
successful build, `--json`'s `null` reference, no "tagged:" line,
resolvable by ID via `ociman inspect`, and `<none>` in `ociman images`
(both text and `--json`). New integration tests: `ociman push` of an
untagged image is a clear error (`ociman_push.rs`); a `save`/`load`
round trip of an untagged image stays untagged in the destination
store, still resolvable and runnable there after an explicit `ociman
tag` (`ociman_load.rs` — `ociman run` has no image-by-ID resolution of
its own at all, a separate, pre-existing, unrelated gap, confirmed
directly by testing a *tagged* image's own short ID identically
failing). Updated one existing unit test in `archive.rs`
(`load_with_no_ref_name_annotation_...`) to assert the new "recorded
under the sentinel, findable by ID" behavior instead of the old
"no pointer at all" one. Full `cargo build --workspace --locked`/
`cargo test --workspace --locked` (2 clean runs)/`cargo fmt --all
--check`/`cargo clippy --workspace --all-targets --locked -- -D
warnings`/`python3 ci/guards.py`/`cargo deny check`/`bash
ci/native-ci.sh` all clean.

## What this doesn't do yet

* `ociman commit` without an `IMAGE` argument — the natural, symmetric
  companion change (real `podman commit` also allows an optional tag),
  reusing this exact same sentinel convention; a separate, later
  increment.
* `cmd_rmi`'s own sibling-list display (`"more than one tag (...)"`)
  shows the raw sentinel string verbatim if an untagged and a tagged
  record ever share a digest — purely cosmetic, not a correctness
  issue (removal by ID still works correctly either way); not fixed
  here.
* Real docker/podman's own *default* `image prune` (no `-a`) targets
  *dangling* (untagged) images specifically — `ociman prune`'s default
  pass still never removes any image pointer at all regardless of tag
  status; matching that fully would be a real, separate behavior
  change to `ociman prune` itself, out of scope here.
* `ociman run <image-id>` — confirmed, directly, to be a separate,
  pre-existing gap unrelated to this increment (`cmd_run`'s own image
  resolution never went through 0122's ID fallback at all, tagged or
  untagged); not fixed here.
