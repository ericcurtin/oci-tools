# Design note 0450: `ociman stats --latest`/`-l`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_stats.rs`.

## What this closes

`ociman stats` had no `--latest`/`-l` flag at all — continuing the
same rollout `0434`-`0437`/`0443`-`0449` already established.

## Real, checked-directly confirmation

`~/git/podman/cmd/podman/containers/stats.go:86,93`: `validate.
AddLatestFlag` — the exact same flag/validation `ociman rm --latest`
(`0434`) already ports. Its own `checkStatOptions` (lines ~97-112)
is a real, genuine three-way mutual exclusivity check:

```go
opts := 0
if statsOptions.All { opts++ }
if statsOptions.Latest { opts++ }
if len(args) > 0 { opts++ }
if opts > 1 {
    return errors.New("--all, --latest and containers cannot be used together")
}
```

Real podman's own `stats` also has a genuinely richer `--all` mode
(stream every real running container's own stats at once, matching
real `docker stats`'s own default when no container is named at all)
— a separate, much larger feature `ociman stats` has never
implemented (it only ever showed exactly one, explicitly-named
container's own stats); this increment's own `--latest` addition
deliberately keeps that same narrower scope, so the ported mutual-
exclusivity check reduces to just `--latest` vs. an explicit
container (the real `--all` third case is simply unreachable here,
matching this project's own already-established "narrow first slice"
precedent every other design note in this series already sets).

A real, checked-directly detail worth noting: real podman's own bare
`podman stats` (no container, no `--latest`, no `--all` at all)
doesn't error — its own doc comment says outright "stats is different
in that it will assume running containers if no input is given",
defaulting to that same richer all-running-containers mode instead.
Since this project doesn't implement that mode, the "neither given"
case here still needs to be a real, immediate error — but there's no
real, matching upstream wording to cite for it (real podman never
takes this exact path at all), so this project's own already-
established "no target given" convention (the same shape `ociman
kill`'s own identical message already uses) is used instead, rather
than inventing something that only looks like it was copied from
real podman.

## Implementation

- `Command::Stats::id` widens from `String` to `Option<String>`
  (omittable when using `--latest`); new `latest: bool`
  (`#[arg(short = 'l', long)]`).
- The dispatch arm checks the (reduced, two-way) mutual exclusivity
  in real podman's own exact wording, then resolves either via
  `resolve_latest_container` or the given `id`; with neither, this
  project's own "no target given" convention (not real podman's own
  wording, which doesn't apply here — see above). `cmd_stats`'s own
  signature is completely unchanged.

## Tests

Four new tests in `tests/tests/ociman_stats.rs` (plus a new
`ociman_run_detached_named`/`wait_for_container_status_by_name` pair,
mirroring `ociman_kill.rs`'s/`ociman_pause.rs`'s own identical
existing helpers, needed for the first time in this file):
`stats_latest_shows_the_most_recently_created_containers_own_stats`
(two named, genuinely running containers with a real, distinguishable
creation-time gap; `stats --latest --no-stream --json` reports only
the newer one's own name, never the older one's), `stats_latest_and_
explicit_id_together_is_a_clear_error`, `stats_with_no_container_
and_no_latest_is_a_clear_error`, and `stats_latest_on_an_empty_
store_is_a_clear_error`. All 8 prior tests in the file pass unmodified
(12/12 total).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (120 test-result
blocks, 0 failures, clean on the first full run), `python3 ci/
guards.py`, `cargo deny check`, `bash ci/native-ci.sh` (clean,
120/120, clean on the first run too), `bash ci/build-deb.sh` (real
`dpkg -i`/`--version`/`dpkg -r` round trip). Touches only `ociman
stats`'s own selection logic, not any hot path at all — no benchmark
re-run needed.

## Deliberately still out of scope

Real podman's own `--all` mode (streaming every running container's
own stats at once) — a separate, genuinely richer feature, not
something this increment's own narrower `--latest` addition needed to
also solve. Continuing this same rollout otherwise: `attach`, `start`,
`port`, and `checkpoint`/`restore` (the last two CRIU-based, a much
larger, separately-scoped gap) still don't have `--latest` here at
all — each a natural, separate future increment.
