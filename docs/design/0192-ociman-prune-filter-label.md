# Design note 0192: `ociman prune --filter label=`

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Prune`'s new `--filter`
flag, `LabelFilter`, `parse_prune_filters`, `cmd_prune`); `tests/tests/
ociman_prune.rs`.

## Starting point

`docs/design/0181`'s own "what this doesn't do yet" named `--filter`
support as a real, open gap. Confirmed real `docker system prune
--filter`/`podman system prune --filter` both document `label=<key>=
<value>` as their own primary example.

## A real, previously-uncharted semantic question, resolved by direct,
repeated, from-a-clean-state testing

Reading `~/git/container-libs/common/libimage/filters.go`'s own
`compileImageFilters`/`applyFilters` first suggested every `--filter`
value (even repeated ones under the same key, e.g. two separate
`--filter label=...`) must independently match — a plain AND. But
direct testing against the actually-*installed* `podman` (4.9.3),
repeated from a genuinely clean image state (no stray dangling images
left over from earlier, unrelated testing — an early attempt was
contaminated by exactly that, and had to be redone from scratch),
showed the opposite: two `--filter label=` values, only one of which
actually matches a given image, still reclaimed it. This was
reproduced three times, including with a completely label-less base
layer in the same prune batch, before trusting it as the real,
authoritative behavior — the installed binary's own behavior is the
ground truth for this project's own "match real podman" goal, not
necessarily whatever a separately-cloned reference repo's own `HEAD`
says, if the two have drifted apart (a real, concrete instance of
exactly that kind of drift, not just a theoretical caveat).

## The fix

* `Command::Prune` gains `--filter` (repeatable `Vec<String>`).
* `parse_prune_filters` accepts only `label=<key>`/`label=<key>=
  <value>`/`label!=<key>`/`label!=<key>=<value>` — any other key (real
  docker/podman also support `until=`/`dangling=`/`reference=`/...) is
  a clear, immediate error, matching real podman's own identical
  refusal for a genuinely unknown filter key (checked directly:
  `podman image prune --filter bogus=xyz` → `Error: unsupported image
  filter "bogus"`).
* `cmd_prune`, for each candidate image already past the existing
  dangling/`--all` check, fetches its config (`Store::image_config`,
  already used elsewhere) and requires *any* (not all) of the given
  label filters to match — matching the real, directly-confirmed OR
  semantics above. With no `--filter` at all, every candidate image
  still qualifies, exactly as before this flag existed.

## Tests

Five new integration tests in `tests/tests/ociman_prune.rs`, reusing
the exact existing dangling-image-via-untagged-`ociman-build` pattern
already established by `prune_without_all_removes_a_dangling_untagged_
image_and_reclaims_its_blobs_too`: exact-value match/mismatch, bare-key
(any value), negation, the OR-across-multiple-filters proof, and an
unsupported filter key. All 11 pre-existing `ociman prune` tests
continue to pass unchanged. Full `cargo build --workspace --locked`/
`cargo test --workspace --locked` (2 clean runs, 83/83 result blocks)/
`cargo fmt --all --check`/`cargo clippy --workspace --all-targets
--locked -- -D warnings`/`python3 ci/guards.py`/`cargo deny check`/
`bash ci/native-ci.sh` all clean. No performance regression (`prune`
is not on the container-launch hot path at all; a routine `ociman run
--rm` sanity check stayed within this project's own normal noise
range).

## What this doesn't do yet

`until=`/`dangling=`/`reference=`/`intermediate=` and every other real
docker/podman filter key remain unsupported — `label=`/`label!=` alone
covers the primary, most commonly documented real-world use case;
extending to the others is a well-scoped, independent future increment
each (`until=` in particular would need a real, persisted creation
timestamp this project's own `ImageRecord` doesn't track at all yet).
