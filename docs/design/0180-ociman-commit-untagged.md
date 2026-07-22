# Design note 0180: untagged `ociman commit`

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Commit`'s `image` field
becomes `Option<String>`, `cmd_commit`/`commit_inner`'s `image`
parameter, `CommitResult.reference` becomes `Option<String>`);
`tests/tests/ociman_commit.rs`.

## Closing the gap 0179 explicitly named as the next increment

0179's own "what this doesn't do yet" section said, verbatim: "`ociman
commit` without an `IMAGE` argument — the natural, symmetric companion
change... reusing this exact same sentinel convention; a separate,
later increment." This increment closes it.

## A direct, mechanical port of 0179's own convention — no new design
needed

`commit_inner` already ends with the identical shape `cmd_build` had
before 0179: parse a required tag string, `put_image` under it, print
`"tagged: ..."`. The fix is the same three-part pattern 0179 already
established for `build`, applied here verbatim:

1. `image: Option<&str>` instead of `&str`; `tag_reference =
   image.map(|image| Reference::parse(image)...).transpose()?`.
2. The reference actually recorded is `tag_reference`'s own string when
   `Some`, or [`untagged_reference`]`(&manifest_ingested.digest)` when
   `None` — the exact same sentinel `ociman build` already uses (a
   bare digest string, never colliding with a real tag, see 0179's own
   doc comment for why).
3. Output: `--json`'s `CommitResult.reference` becomes `Option<String>`
   (`null` when untagged); the plain-text path prints only the digest,
   with no `"tagged: ..."` line at all when there's no tag to report —
   matching real `podman commit` with no `IMAGE` exactly.

No other code needed to change at all: `cmd_push`'s untagged-image
guard, `ImageView`'s `<none>` display, `save`/`load`'s tag-omission
symmetry, and `resolve_image_by_reference_or_id`'s real-tag-wins tie-
break (0179) are all shared, reference-agnostic machinery that already
works for any `ImageRecord` regardless of which command produced its
sentinel-shaped reference — `commit`'s own untagged output is
immediately just as inspectable/pushable-after-tagging/saveable/
listable as `build`'s own was.

## Manual, real, end-to-end verification

Before writing automated tests, the full lifecycle was run manually
against the real built binary: `ociman commit <container>` (no
`IMAGE`) succeeds and shows in `ociman images` as `<none>`; `ociman
push <its-id>` fails with the same clear `"cannot push an untagged
image"` error 0179 added; a `save`/`load` round trip into a fresh
store preserves the untagged state on the far end (still `<none>`,
still resolvable by ID). The pre-existing `"Loaded image: {digest}"`
fallback text (for an archive whose `references` come back empty) was
confirmed to already read sensibly for this case too — no change
needed there, it already existed before 0179/0180 for exactly this
shape of load.

## Tests

`tests/tests/ociman_commit.rs` gained one integration test
(`commit_with_no_image_argument_records_an_untagged_image`): a commit
with no `IMAGE` succeeds, prints no `"tagged: ..."` line, `--json`
shows `"reference": null`, the result is findable by ID via `ociman
inspect`, and shows up as `<none>` in both `ociman images`'s text and
`--json` output. The one existing test that already exercises a bad
(not absent) tag string
(`commit_requires_the_image_argument_to_parse_as_a_reference`) needed
no changes — it still correctly fails on `Reference::parse`'s own
rejection of `"Not A Valid Tag!!"`, an orthogonal case from omitting
the argument entirely. Full `cargo build --workspace --locked`/`cargo
test --workspace --locked` (2 clean runs)/`cargo fmt --all --check`/
`cargo clippy --workspace --all-targets --locked -- -D warnings`/
`python3 ci/guards.py`/`cargo deny check`/`bash ci/native-ci.sh` all
clean.

## What this doesn't do yet

Nothing new — this increment closes 0179's own last-named gap for
`ociman commit` completely; the remaining "what this doesn't do yet"
items from 0179 itself (`cmd_rmi`'s cosmetic sibling display, `ociman
prune`'s default dangling-image behavior, `ociman run <image-id>`)
are unrelated to `commit` and still apply exactly as 0179 described
them.
