# Design note 0275: `ociman ps --filter label=`/`label!=`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_ps.rs`.

## Closing the last research gap from `0272`/`0274`

`0272` deferred `ociman ps --filter label=` twice over: first pending
more direct verification of a confusing real result, then (in `0274`)
pending container-level labels existing at all. Both prerequisites are
now resolved (`0274`'s own research note): the earlier confusing
result was test contamination, not a real anomaly, and `ociman run`/
`create --label` now gives containers real labels of their own to
filter on. This note closes the filter itself.

## Semantics, re-verified cleanly against a real installed `podman`

Redone end to end from a fully clean slate (`podman ps -a`/`podman
images` both empty before starting, labels double-checked via `podman
inspect` immediately after creating each test container):

- A single `label=key=value` matches only a container whose own real
  labels have that exact key/value.
- Multiple `label=` values are **ANDed together** — every one must be
  satisfied by the *same* container (confirmed: two jointly-
  satisfiable values still matched only the one container satisfying
  both; two jointly-unsatisfiable values found nothing at all, not the
  container satisfying just one of them). This is a genuinely
  *different* combination rule than `ociman prune --filter label=`'s
  own already-shipped OR semantics (`0192`) — real podman's own
  container-specific `MatchLabelFilters` (used by `ps`) really is a
  different function than its image-specific `filterLabel`/OR-
  combining logic (used by `image prune`), not a project
  inconsistency to reconcile.
- `label!=key=value` negates: matches every container *except* one
  with that exact key/value.
- A bare `label=key` (no value) matches any container with that key
  present, regardless of its value.
- Like `id=`/`name=` (`0273`) and unlike `status=` (`0272`), giving
  `label=`/`label!=` does **not** override the default running-only/
  `--all` visibility rule on its own — checked directly: `podman ps
  --filter label=env=prod` (no `-a`) still hides a matching but
  non-running container.

## Implementation

Reuses the exact same `LabelFilter`/`try_parse_label_filter` machinery
`ociman prune`/`ociman images --filter` already share (`0192`/`0268`)
for parsing — the *combination* logic is the only real difference,
using `.iter().all(...)` (AND) here instead of the `.iter().any(...)`
(OR) those two commands use, matching this project's own now fully
research-backed understanding of the two genuinely different real
upstream behaviors. Filters against the container's own real,
effective label set (`ANNOTATION_LABELS`, `0274`) — image-inherited
labels plus any explicit `--label`, exactly what `ociman inspect`'s
own `labels` field already shows.

## Verified

Integration (`tests/tests/ociman_ps.rs`, one new, thorough test):

- A single `label=` matches only the one container with that label.
- Two jointly satisfiable values (both true for one container) still
  match only that one container.
- Two jointly unsatisfiable values (true for none of the same
  container) find nothing — confirming a real AND, not a silent OR
  that would otherwise still find a partial match.
- `label!=` negates correctly.
- A bare key matches any value.
- `label=` alone (no `-a`) still respects the default running-only
  visibility rule, unlike `status=`.

Regression: all 16 pre-existing `ociman_ps.rs` tests still pass (one,
`ps_filter_with_an_unrecognized_key_or_value_is_a_clear_error`,
updated to use a still-genuinely-unsupported key, `ancestor=`, since
its previous example, `label=`, is now supported).

Full workspace: `cargo build`/`test --workspace`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`python3 ci/guards.py`, `cargo deny check`, `ci/native-ci.sh`,
`ci/build-deb.sh`.

## Still ahead

Real podman's own larger `ps --filter` family (`ancestor`,
`before`/`since`, `pod`, `network`, `restart-policy`, `command`,
`annotation`/`annotation!`, `exited`, `health`, `volume`, `until`)
remains further, separately-scoped candidates. `--label-file <path>`
for `ociman run`/`create` (real podman/docker's own sibling flag to
`--label`) also remains a smaller candidate.
</content>
</invoke>
