# Design note 0280: `ociman ps --filter before=`/`since=`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_ps.rs`.

## Continuing the `ps --filter` family

`0272`/`0273`/`0275` implemented `status=`/`id=`/`name=`/`label=`.
`before=`/`since=` (compare a container's own creation time against
*another* container's) is the next real, well-scoped candidate from
real podman's own larger `ps --filter` family.

## An `ocibox upgrade` investigation, and a correction

Before picking this up, `ocibox upgrade` was reconsidered — an earlier
research pass had suggested real `distrobox upgrade`'s own Go
implementation (`pkg/commands/upgrade.go`) might be simpler than
`0211` originally judged, since `Execute` itself is just `enter` with
a fixed command. Reading further this time: that fixed command is `sh
-c '... /usr/bin/entrypoint --upgrade'` — a real per-distro package-
manager-detecting script real `distrobox create` bakes into every box
via its own init/entrypoint mechanism, which `ocibox create` doesn't
have any equivalent of at all. `0211`'s original deferral was correct;
the earlier research's "simpler than thought" conclusion was based on
reading `upgrade.go` alone without following through to what its own
`entrypoint --upgrade` invocation actually depends on. `ocibox
upgrade` remains real, correctly-deferred, separately-scoped work
needing that whole entrypoint/package-manager subsystem first, not a
small addition — noted here so this incorrect impression doesn't
resurface and get acted on by mistake in a future turn.

## Real semantics, checked directly against `pkg/domain/filters/containers.go`

- `before=<container>`/`since=<container>`: each value names *another*
  container (by id or `--name`, same resolution `ociman rm`/etc.
  already use), whose own creation time becomes the comparison point.
  `before=X` keeps only containers created *strictly earlier* than
  `X`'s own creation time; `since=X`, *strictly later*.
- Multiple values for the same key use the **earliest** of all the
  given reference containers' own creation times — real podman's own
  source (`if createTime.IsZero() || createTime.After(ctr.CreatedTime())`,
  identical for both `before` and `since`) always keeps the minimum,
  regardless of which key. Re-verified directly rather than trusted
  from a first source read alone (this project's own track record of
  finding real multi-value quirks made a second check worthwhile):
  three real containers, two seconds apart; `before=ctr2 --filter
  before=ctr3` produced the identical result as `before=ctr2` alone
  (`ctr3`'s own, later creation time never wins).
- An unresolvable reference container is a clear error.
- Like `id=`/`name=`/`label=` and unlike `status=`, giving `before=`/
  `since=` does **not** override the default running-only/`--all`
  visibility rule on its own — checked directly.

## Implementation

Each reference container is resolved once, up front in `cmd_ps`
(needs a real store lookup, so it can't happen inside the per-
container filter closure, which must stay infallible) via the exact
same `resolve_container_id` helper `ociman rm`/etc. already share, then
its own `created` field (already an RFC3339 string on every
`PersistedState`) is parsed with the same `oci_spec_types::time::
parse_rfc3339_utc` `ociman prune --filter until=` (`0198`) already
uses. The earliest of a filter's own multiple reference containers is
computed once via a small `try_fold`, then compared per container
inside the existing filter closure — no new parsing/comparison
primitive needed beyond what already existed.

## Verified

Integration (`tests/tests/ociman_ps.rs`, one new test):

- `before=ctr2` finds only the container created before it;
  `since=ctr2` only the one created after.
- Multiple `before=` values (`before=ctr2 --filter before=ctr3`)
  produce the identical result as `before=ctr2` alone, confirming the
  earliest-of-the-references rule.
- An unresolvable reference container is a clear error.
- `before=`/`since=` alone (no `-a`) don't override the default
  running-only visibility rule, unlike `status=`.

Regression: all 17 pre-existing `ociman_ps.rs` tests still pass
unmodified.

Full workspace: `cargo build`/`test --workspace`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`python3 ci/guards.py`, `cargo deny check`, `ci/native-ci.sh`,
`ci/build-deb.sh`.

## Still ahead

Real podman's own remaining `ps --filter` family (`ancestor`, `pod`,
`network`, `restart-policy`, `command`, `annotation`/`annotation!`,
`exited`, `health`, `volume`, `until`) remains further, separately-
scoped candidates.
</content>
</invoke>
