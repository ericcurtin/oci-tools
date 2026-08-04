# Design note 0452: `ociman attach --latest`/`-l`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_attach.rs`.

## What this closes

`ociman attach` had no `--latest`/`-l` flag at all — continuing the
same rollout `0434`-`0437`/`0443`-`0451` already established.

## Real, checked-directly confirmation

`~/git/podman/cmd/podman/containers/attach.go:60,67`: `validate.
AddLatestFlag` — the exact same flag/validation `ociman rm --latest`
(`0434`) already ports. Its own `attach` function (lines ~70-85) is
a real, checked-directly divergence from most other siblings in this
rollout, the identical shape `ociman diff --latest` (`0448`) already
established:

```go
if len(args) > 1 || (len(args) == 0 && !attachOpts.Latest) {
    return errors.New("attach requires the name or id of one running container or the latest flag")
}
var name string
if len(args) > 0 {
    name = strings.TrimPrefix(args[0], "/")
}
```

There is **no mutual-exclusivity check at all** between `--latest`
and an explicit `ID` — if an argument is given at all, `name` is set
from it unconditionally (`attachOpts.Latest` is simply never consulted
in that branch), so an explicit id always silently wins outright,
exactly like `diff`. Giving neither is still a real, immediate error,
in the exact wording above.

## Implementation

- `Command::Attach::id` widens from `String` to `Option<String>`
  (omittable when using `--latest`); new `latest: bool`
  (`#[arg(short = 'l', long)]`).
- The dispatch arm mirrors real podman's own exact precedence: an
  explicit `id`, when given, is always used verbatim, regardless of
  `--latest` (ported faithfully — no stricter mutual-exclusivity
  check added that real podman itself doesn't have); with no `id`,
  `--latest` resolves via `resolve_latest_container`; with neither, a
  real, immediate error in real podman's own exact wording.
  `cmd_attach`'s own signature is completely unchanged.

## Tests

Four new tests in `tests/tests/ociman_attach.rs`: `attach_latest_
streams_the_most_recently_created_running_container` (two
independent running containers with a real, distinguishable
creation-time gap, each echoing genuinely different output before
sleeping briefly; `attach --latest` streams only the newer one's own
output, never the older one's), `attach_explicit_id_silently_wins_
over_latest_when_both_given` (a direct, convincing proof of the real
"no mutual exclusivity, explicit always wins" quirk above),
`attach_with_no_id_and_no_latest_is_a_clear_error`, and `attach_
latest_on_an_empty_store_is_a_clear_error`. All 3 prior tests in the
file pass unmodified (7/7 total).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (120 test-result
blocks, 0 failures, clean on the first full run), `python3 ci/
guards.py`, `cargo deny check`, `bash ci/native-ci.sh` (clean,
120/120, clean on the first run too), `bash ci/build-deb.sh` (real
`dpkg -i`/`--version`/`dpkg -r` round trip). Touches only `ociman
attach`'s own selection logic, not any hot path at all — no
benchmark re-run needed.

## Deliberately still out of scope

Continuing this same rollout: `port`, and `checkpoint`/`restore` (the
latter CRIU-based, a much larger, separately-scoped gap) still don't
have `--latest` here at all — `port` in particular is a real,
networking-specific command this project has never implemented at
all either (no separate network namespace concept, see this
project's own already-established architecture notes), so `--latest`
there would need `ociman port` to exist first, a separate, unrelated
gap entirely.
