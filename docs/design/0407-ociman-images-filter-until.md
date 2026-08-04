# Design note 0407: `ociman images --filter until=<duration-or-timestamp>`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_images.rs`,
`README.md`.

## What this closes

A real, previously-mis-scoped gap: `ImageFilters`'s own doc comment
claimed `until` was deliberately excluded from `ociman images
--filter` because "prune-specific semantics don't apply to a plain
listing." Real `podman images --filter until=` genuinely exists and
is documented, sharing the identical underlying time-parsing
machinery `podman prune --filter until=` already uses.

## Real, checked-directly confirmation

- `~/git/podman/docs/source/markdown/podman-images.1.md.in`: "The
  `until` filter accepts formats: golang duration, RFC3339 time, or a
  Unix timestamp and shows all images that are created until that
  time."
- `~/git/container-libs/common/libimage/filters.go`'s own
  `compileImageFilters` (used by `(*Runtime).ListImages`, real
  `podman images`'s own actual data source) has a `case "until":`
  branch identical in shape to every other filter key, calling
  `r.until(value)` then `filterBefore(until)` — `img.Created().
  Before(value)`, a strict "created before this real, absolute
  instant" comparison.
- `r.until`'s own real value computation (`filters.go`): for a plain
  duration string, the threshold is `time.Now()` minus the given
  duration (i.e., `until=24h` means "created before 24 hours ago" —
  matches images *older* than that, not images created within the
  last 24 hours, a real, easy-to-get-backwards semantic caught while
  writing this note's own test, see below).

## Implementation

- A new, shared `parse_until_filter_value(command, f, rest)` factored
  out of `parse_prune_filters`/`parse_ps_filters`'s own previously
  near-duplicated (three-copies-once-`images`-needed-it-too) inline
  `until=` parsing — the same "shared primitive factored out the
  moment a third caller needs it" move this project already makes
  routinely (e.g. `try_parse_label_filter`/`try_parse_dangling_
  filter`, both already shared by `prune`/`images`). Both existing
  callers refactored to use it, verified as a genuine, zero-behavior-
  change move (`ociman_prune.rs`'s 5 `until=`-specific tests and
  `ociman_ps.rs`'s 1 pass completely unmodified).
- `ImageFilters` gains `until: Option<std::time::SystemTime>`;
  `parse_image_filters` gains an `until=` branch (identical shape to
  `prune`/`ps`'s own, now calling the shared helper); `cmd_images`'s
  own per-record filter loop folds the same strict `created >=
  threshold -> skip` check into its existing `before=`/`since=`
  config-reading block (all three need the same `config.created`
  field, so they share the one `needs_config`/lookup).
- Corrected the stale, incorrect `ImageFilters` doc comment.

## A real semantic trap, caught by writing the test rather than assumed

The first version of this note's own integration test asserted a
freshly built image (created a fraction of a second ago) should
*match* `--filter until=24h` — it does not, and the assertion failed
immediately against the real implementation. `until=<duration>`
means "older than `<duration>` ago" (`created < now - duration`), the
*opposite* of "created within the last `<duration>`" — an easy,
plausible-sounding misreading of the flag's own name. Caught by the
test failing, not by re-reading documentation more carefully after
the fact; fixed in the test (the implementation was already correct,
having reused the already-proven `prune`/`ps` logic verbatim).

## Tests

Two new end-to-end integration tests in `tests/tests/ociman_images.rs`:
`images_filter_until_matches_images_created_strictly_before_the_threshold`
(a real, freshly built image correctly *excluded* by a `24h`-ago
threshold, then correctly *included* once a `1s`-ago threshold is
given after a real 2-second sleep) and `images_filter_until_rejects_
more_than_one_value` (matching `ps`/`prune`'s own identical refusal).
All existing tests continue to pass unmodified (20/20 in
`ociman_images.rs`, 26/26 in `ociman_prune.rs`, 49/49 in
`ociman_ps.rs`).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures), `python3
ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh`, `bash
ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg -r` round trip).
This touches only `ociman images`'s own filter-matching path, not
`launch.rs`'s hot path at all — no benchmark re-run needed.

## Deliberately still out of scope

`manifest=true|false` and `intermediate=` remain unimplemented — each
needs real, unbuilt machinery (a fat-manifest-list storage concept;
a parent/child layer-tree query) this project's own single-platform-
per-pull, flat image model has no equivalent of at all, a real,
separate, bigger gap matching this project's own already-documented
scope limits (`docs/design/0350`).
