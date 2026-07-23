# Design note 0197: `ociman build` — fix `created` never updating

Status: implemented
Scope: `bin/ociman/src/build.rs` (`cmd_build`'s new `config.created =
...` assignment); `tests/tests/ociman_build.rs`.

## Continuing milestone 4

While scoping `ociman prune --filter until=` (named as a remaining
milestone-4 survey item in 0196's own "what this doesn't do yet"),
investigating what `until=` actually needs to compare against — an
image's own `created` timestamp — surfaced a real, previously-
unnoticed, more fundamental bug worth fixing first and on its own:
`ociman build` never actually updates the built image's own top-level
`created` field anywhere in this file. Every image it has ever
produced has silently kept its base image's own original `created`
value forever, no matter how many `RUN`/`COPY`/`LABEL` instructions
(or `--label` flags) actually ran. Implementing `until=` on top of
that would have made the filter itself subtly wrong for the
overwhelmingly common case (comparing a built image's own build time
against a threshold, when what's actually stored is its unrelated
base image's own pull/build time) — so this is deliberately its own
increment, `until=` deferred to a following one.

## Real, checked-directly semantics

Verified directly against a real installed `podman build`, three
scenarios (each isolated, `--no-cache`, fresh containers):

1. A `RUN echo hi > /hi.txt` build: the built image's own `Created`
   exactly matches its own last `podman history` entry's own real
   timestamp (down to sub-second precision) — a real, current
   timestamp, not the base's.
2. A bare `FROM busybox` with *no* instructions at all and no
   `--label`: `Created` is left completely unchanged from the base —
   there is no new history entry to update it with, a true no-op.
3. `--label extra=onecommand` alone (no `RUN`/`COPY`/`ADD` at all):
   still bumps `Created`, exactly matching scenario 1 — `--label`
   itself adds its own trailing history entry (already an established,
   pre-existing behavior of this project's own `ociman build --label`,
   see `docs/design/0135`), and that alone is enough.

So the real rule: a built image's own `created` always mirrors its
own *last* history entry's own `created`, whatever produced it — a
real instruction or a CLI-flag-driven one (`--label`). A stage that
adds no new history entry of its own at all inherits the base's
`created` completely unchanged.

## The fix

This project's own `oci_dockerfile::record_layer`/
`record_empty_history` already correctly timestamp every history entry
they append with the real, current time
(`oci_spec_types::time::format_rfc3339_utc(SystemTime::now())`) — that
part was never broken. The only missing piece was ever propagating the
*last* one back up to the top-level `ImageConfig.created` field
`ociman inspect`/`docker inspect .Created` actually reads. One new
line, added in `cmd_build` right after every other final-config
mutation (`--label`, `--unsetenv`, `--unsetlabel`) and right before
serializing/ingesting the config:

```rust
config.created = config.history.last().and_then(|entry| entry.created.clone());
```

Correct for every case at once, with no branching needed: an untouched
bare `FROM`'s own last history entry is already whatever `created` its
base already had (a real no-op), `--squash`/`--squash-all` (which run
inside `build_stage`, *before* this line, and preserve each entry's own
original timestamp while only truncating/relabeling which ones survive
— see `build_stage`'s own squash post-processing) still leave the
truly-last surviving entry's own timestamp intact, and `--label`'s own
trailing entry (added in `cmd_build` itself, before this line runs)
is naturally the new last entry whenever present.

## Tests

Three new integration tests in `tests/tests/ociman_build.rs`,
covering all three real scenarios above — `seed_image`'s own base
config always has `created: None`, so a built image whose own
`created` comes back `Some(...)` at all can only be explained by this
fix actually running:
* `build_updates_created_to_match_the_last_history_entry_after_a_run_step`
  — a `RUN` step bumps `created` to a real, current timestamp (within
  30 seconds of the test's own recorded "before" time).
* `build_leaves_created_unchanged_for_a_bare_from_with_no_instructions`
  — a bare `FROM` alone leaves `created` exactly `None`, matching the
  base's own (no regression for the common "just re-tag" case).
* `build_label_flag_alone_also_bumps_created` — `--label` alone (no
  real instructions) is enough on its own.

All 105 pre-existing `ociman build` tests continue to pass unchanged
(108 total now). Full `cargo build --workspace --locked`/`cargo test
--workspace --locked` (2 clean runs, 83/83 result blocks)/`cargo fmt
--all --check`/`cargo clippy --workspace --all-targets --locked -- -D
warnings`/`python3 ci/guards.py`/`cargo deny check`/`bash
ci/native-ci.sh` all clean. No performance regression (`ociman build
--no-cache`, one `RUN` step, ~17.8ms, consistent with prior
measurements for the same scenario).

## What this doesn't do yet

`ociman prune --filter until=`/`dangling=` (the item that surfaced
this bug while being scoped) remains open, now unblocked by this fix
and a natural next increment. `--timestamp` (reproducible builds) and
the larger `RUN --mount=`/heredoc/multi-platform gaps named by the
earlier milestone-4 survey also remain open.
