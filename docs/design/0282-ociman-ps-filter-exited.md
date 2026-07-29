# Design note 0282: `ociman ps --filter exited=`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_ps.rs`.

## Continuing the `ps --filter` family

`0272`/`0273`/`0275`/`0280`/`0281` implemented `status=`/`id=`/
`name=`/`label=`/`before=`/`since=`/`ancestor=`. `exited=<code>`
(filter by a stopped container's own real exit code) is the next
real, well-scoped candidate.

## Real semantics, checked directly

`pkg/domain/filters/containers.go`'s own "exited" case: parses each
value as a signed 32-bit integer, matches a container whose own real,
recorded exit code equals one of them — and *only* a container that
has actually exited at all (never a still-running or never-started
one, regardless of what code happens to be given). Verified directly
against the real *installed* podman (`4.9.3`, matching this note's own
predecessor's discovery that the locally cloned `~/git/podman` source
is a materially newer, sometimes-different `v5.4.0-rc1`): three real
containers exiting `0`/`5`/`7`, `--filter exited=5` finding only the
one that exited `5`, `--filter exited=5 --filter exited=7` (multiple
values) finding both (OR'd together, same convention every other
multi-value `ps --filter` key here already uses), and — like every
other filter key here except `status=` — giving `exited=` alone (no
`-a`) does not override the default running-only visibility rule.

## Implementation

This project already records a container's own real exit code as
`ANNOTATION_EXIT_CODE` (used by `ContainerView`/`ContainerInspectView`
already) — no new storage needed. A container that never recorded one
(still running, or never started) simply never matches any `exited=`
value, matching real podman's own identical "must have actually
exited" rule.

## Verified

Integration (`tests/tests/ociman_ps.rs`, one new test):

- A single `exited=` value matches only the container with that exact
  exit code.
- Multiple values are OR'd together.
- `exited=0` correctly matches a container that exited successfully
  (not confused with "hasn't exited").
- A non-matching code finds nothing; a non-numeric value is a clear
  error.
- `exited=` alone (no `-a`) doesn't override the default running-only
  visibility rule, unlike `status=`.

Regression: all 19 pre-existing `ociman_ps.rs` tests still pass
unmodified.

Full workspace: `cargo build`/`test --workspace`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`python3 ci/guards.py`, `cargo deny check`, `ci/native-ci.sh`,
`ci/build-deb.sh`.

## Still ahead

The rest of real podman's own remaining `ps --filter` family (`pod`,
`network`, `restart-policy`, `command`, `annotation`/`annotation!`,
`health`, `volume`, `until`) remains further, separately-scoped
candidates.
</content>
</invoke>
