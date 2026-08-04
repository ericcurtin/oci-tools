# Design note 0438: `ociman images --sort`

Status: implemented
Scope: `crates/oci-store/src/describe.rs`, `bin/ociman/src/main.rs`,
`tests/tests/ociman_images.rs`.

## What this closes

`ociman images` had no `--sort` flag at all, and — a real,
previously-unnoticed default-ordering gap, not merely a missing flag —
its own default listing order was alphabetical by reference (an
incidental consequence of `Store::list_images`'s own on-disk pointer
sort, `crates/oci-store/src/images.rs`), never matching real `podman
images`'/`docker images`'s own actual default order at all.

## Real, checked-directly confirmation

`~/git/podman/cmd/podman/images/list.go`:

- `listFlag.sort = "created"` is set **unconditionally**, before flag
  parsing even runs (line ~98) — real podman always sorts by creation
  time, newest first, even when `--sort` itself is never given. This
  project's own pre-existing alphabetical default never matched that.
- The real, valid choice list (`validate.Value`, line ~100-106):
  `created`, `id`, `repository`, `size`, `tag` — five keys, a real,
  deliberate, smaller set than `ociman ps --sort`'s own eight (`0386`);
  real podman's own `images --sort` has no `command`/`names`/`status`/
  `runningfor` equivalent at all (an image has none of those concepts).
- `sortImages`'s own `slices.SortFunc` (line ~257-278): every key
  sorts ascending via `cmp.Compare` **except** `created`, which
  explicitly sorts descending (newest first) — a real, checked-
  directly asymmetry from `ociman ps --sort`'s own already-established
  convention (every one of *its* eight keys sorts ascending, `0386`).
- `imageReporter.ID()` (line ~349): the same short, 12-hex-char digest
  prefix this project's own `-q`/table `DIGEST` column already shows
  (`sha256:`-prefixed only as a fallback when the digest happens to be
  under 12 characters, an unreachable case for a real sha256 digest).
- `tokenRepoTag` (line ~283-300): splits a tag off a reference at the
  last `:` (provided it doesn't cross a `/`, the same real rule this
  project's own `Reference::parse` already implements); `repository`
  ties break on `tag`, also ascending.

## Implementation

- `oci_store::ImageSummary` gains a `created: Option<String>` field
  (the image config's own `created`, RFC3339) — genuinely free to add:
  `Store::image_summary` already unconditionally parses the config
  blob for `architecture`/`os`, so this is one more field read from
  data already in hand, no extra blob read.
- `ImageView` (the JSON/table view shared by `pull` and `images`)
  gains the same `created: Option<String>` field, populated straight
  from the summary.
- New `ImagesSortKey` enum (`clap::ValueEnum`, the same established
  derive convention `PsSortKey`/`PullPolicy`/`SaveFormat` already use).
- `Command::Images` (and its `ociman image list`/`ls` alias, `0430`)
  gain `sort: Option<ImagesSortKey>`.
- `cmd_images` treats `None` (no `--sort` given) identically to
  `Some(ImagesSortKey::Created)`, matching real podman's own always-on
  default exactly — a real, deliberate difference from `ociman ps
  --sort`'s own `None` (which applies no extra sort at all, relying on
  a separate, already-correct default `views.sort_by` a few lines
  above it, `0386`): `ociman images` had no such already-correct
  default sort to fall back on, so the fallback is folded directly
  into the same `match`.
- A small local `repo_and_tag` closure splits `ImageView::reference`
  into repository/tag for the `repository`/`tag` keys, using a plain
  rightmost-colon split (this project's own reference strings are
  always already fully normalized with an explicit tag when tagged at
  all, so no need for a full `Reference::parse` re-run); an untagged
  image (`reference: None`) gets `<none>`/`<none>`, matching real
  podman's own identical sentinel for the same case.

## Tests

Seven new tests in `tests/tests/ociman_images.rs`:
`images_default_order_and_explicit_sort_created_are_both_newest_first`
(three real, distinct-creation-time images via `ociman build`, plus a
`seed_image`-only base image with no recorded `created` at all, which
sorts last, treated as the oldest possible image — both the default,
flagless listing and an explicit `--sort created` give the identical
newest-first order), `images_sort_id_orders_ascending_by_short_digest`,
`images_sort_repository_orders_ascending_and_ties_break_on_tag`,
`images_sort_size_orders_ascending_by_byte_count` (three images whose
own reference names are deliberately *not* already in size order, a
real proof this genuinely re-sorts by size), `images_sort_tag_orders_
ascending_by_tag_alone` (a tag whose own repository would sort *last*
still sorts first here, proving this is tag-alone, not repository-
then-tag), `images_sort_rejects_an_invalid_value`, and `image_list_
sort_is_the_same_flag_images_sort_is` (the `0430` alias). Two
pre-existing tests in the same file needed an ordering-expectation fix
(not a behavior change of their own): `images_filter_before_and_since_
use_the_referenced_images_own_creation_time`'s own `before=`/`since=`
result lists were asserted in the old, incidental alphabetical order,
now correctly newest-first. All 21 prior tests (post-fix) plus 7 new
= 28/28 pass.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (119 test-result
blocks, 0 failures on a clean run — one earlier run hit the pre-
existing, previously-documented `ocicri_container.rs` host-contention
flakiness from the long-running runaway CPU-spinning process on this
host, confirmed unrelated by rerunning that one test in isolation,
then the full suite again cleanly), `python3 ci/guards.py`, `cargo
deny check`, `bash ci/native-ci.sh` (clean, 119/119), `bash
ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg -r` round trip). Not
on any container-launch hot path `ci/bench.sh` measures (confirmed by
grep: `bench.sh` only ever calls `ociman images` as a plain existence
check after a build, never under timing) — the added per-listing cost
itself is also negligible, an in-memory `O(n log n)` sort over
summaries already fully computed, plus one already-parsed struct
field — no benchmark re-verification needed.

## Deliberately still out of scope

Real `podman images`'s own `--history`/`--digests`/`--no-trunc`/
`--noheading` flags remain unported (this project's own plain table
has no history/digests columns to control at all, and no heading to
suppress in the first place — a narrower table than real podman's
own, an existing, separately-scoped gap, not something this increment
introduces).
