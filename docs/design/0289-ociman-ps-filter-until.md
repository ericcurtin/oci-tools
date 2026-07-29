# Design note 0289: `ociman ps --filter until=`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_ps.rs`.

## Continuing the `ps --filter` family

`0272`/`0273`/`0275`/`0280`/`0281`/`0282` implemented `status=`/`id=`/
`name=`/`label=`/`before=`/`since=`/`ancestor=`/`exited=`. `until=` is
the next real, well-scoped candidate from real podman's own larger
`ps --filter` family, and (unlike most of the others) needed no new
parsing or comparison primitive at all: `ociman prune --filter until=`
(`0198`) already established the exact duration-or-absolute-timestamp
threshold computation this reuses verbatim.

## Real semantics, checked directly against `pkg/domain/filters/containers.go`

`~/git/podman/pkg/domain/filters/containers.go`'s own
`prepareUntilFilterFunc` delegates straight to
`~/git/podman/vendor/go.podman.io/common/pkg/filters/filters.go`'s
`ComputeUntilTimestamp`:

- Exactly one value is accepted (`len(filterValues) != 1` is a hard
  error, `"specify exactly one timestamp for until"`) — the same
  single-value-only rule `ociman prune --filter until=` already
  enforces.
- The match itself is `c.CreatedTime().Before(until)` — **strictly**
  before the threshold, never inclusive.
- Confirmed directly against a real installed `podman ps` too: a
  stopped container matching `until=24h` stays hidden without `-a`
  (unlike `status=`, `until=` is an ordinary additional constraint,
  not an override of the default running-only visibility rule) —
  the same rule every `ps --filter` key except `status=` already
  follows here (`id=`/`name=`/`label=`/`before=`/`since=`/`ancestor=`/
  `exited=`).

## Implementation

Reuses `parse_prune_filters`'s own exact `until=` branch (single-value
check, `parse_simple_duration` for a relative duration like `24h`,
`oci_spec_types::time::parse_rfc3339_utc` for an absolute timestamp,
`now.checked_sub(duration)` for the threshold) verbatim in
`parse_ps_filters`, and the same strict-`Before` comparison
`before=`/`since=` (`0280`) already established for creation-time
comparisons in `cmd_ps`'s own filter closure — genuinely zero new
logic, purely composing two already-tested primitives from two
different existing commands.

## Verified

Integration (`tests/tests/ociman_ps.rs`, one new test):

- A far-future absolute timestamp (`2999-01-01T00:00:00Z`) matches
  every container.
- A far-past one (`1970-01-01T00:00:00Z`) matches none.
- A relative duration (`24h`) matches none, since the test containers
  were all created well within the last second.
- More than one `until=` value is a clear error.
- A value that's neither a duration nor an RFC3339 timestamp is a
  clear error.
- `until=` alone (no `-a`) doesn't override the default running-only
  visibility rule, unlike `status=`.

Regression: all 20 pre-existing `ociman_ps.rs` tests still pass
unmodified.

Full workspace: `cargo build`/`test --workspace` (111 test binaries),
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets --
-D warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh`.

## Still ahead

Real podman's own remaining `ps --filter` family (`pod`, `network`,
`restart-policy`, `command`, `annotation`/`annotation!`, `health`,
`volume`, `should-start-on-boot`) remains further, separately-scoped
candidates.
