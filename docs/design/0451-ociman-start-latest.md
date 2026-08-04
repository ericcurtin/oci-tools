# Design note 0451: `ociman start --latest`/`-l`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_start.rs`.

## What this closes

`ociman start` had no `--latest`/`-l` flag at all — continuing the
same rollout `0434`-`0437`/`0443`-`0450` already established.

## Real, checked-directly confirmation

`~/git/podman/cmd/podman/containers/start.go:75,82`: `validate.
AddLatestFlag` — the exact same flag/validation `ociman rm --latest`
(`0434`) already ports. Its own `validateStart` (lines ~84-102) is a
real, larger, multi-flag validation matrix (`--all`, `--filter`,
`--latest`, multi-target `--attach`), of which only two rules are
actually reachable here, given `ociman start` has never implemented
real podman's own `--all`/`--filter`/multi-target support (a
separate, richer scope, deliberately still out of scope — the same
narrower-first-slice precedent every other design note in this
project already sets):

```go
if len(args) == 0 && !startOptions.Latest && !startOptions.All && len(filters) < 1 {
    return errors.New("start requires at least one argument")
}
if len(args) > 0 && startOptions.Latest {
    return errors.New("--latest and containers cannot be used together")
}
```

## Implementation

- `Command::Start::id` widens from `String` to `Option<String>`
  (omittable when using `--latest`); new `latest: bool`
  (`#[arg(short = 'l', long)]`).
- The dispatch arm ports the two reachable rules above, in real
  podman's own exact wording, then resolves either via `resolve_
  latest_container` or the given `id`. `cmd_start`'s own signature is
  completely unchanged — `--attach` composes with `--latest` exactly
  the same way it already did with an explicit id.

## Tests

Four new tests in `tests/tests/ociman_start.rs`: `start_latest_
starts_only_the_most_recently_created_container` (two containers via
`create` — never started — with a real, distinguishable creation-
time gap, each writing to its own real per-container rootfs marker
file the moment it actually runs; `start --latest` writes only the
newer one's own marker, leaving the older one's rootfs untouched and
its own status still `created`, never started, a real, convincing
proof rather than merely "some start succeeded"), `start_latest_and_
explicit_id_together_is_a_clear_error`, `start_with_no_container_
and_no_latest_is_a_clear_error`, and `start_latest_on_an_empty_
store_is_a_clear_error`. All 24 prior tests in the file pass
unmodified (28/28 total).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (120 test-result
blocks, 0 failures, clean on the third full run — the first two each
hit one different instance of the pre-existing, previously-documented
host-contention flakiness from the long-running runaway CPU-spinning
process on this host (`ociman_logs.rs`, then `ocicri_container.rs`),
each confirmed unrelated and transient by an immediate isolated
rerun), `python3 ci/guards.py`, `cargo deny check`, `bash
ci/native-ci.sh` (clean, 120/120 — three consecutive earlier runs
each hit a different instance of the identical class of `ocicri_
container.rs` flakiness, each confirmed transient the same way, then
a clean full rerun), `bash ci/build-deb.sh` (real `dpkg -i`/
`--version`/`dpkg -r` round trip). Touches only `ociman start`'s own
selection logic, not any hot path at all — no benchmark re-run
needed.

## Deliberately still out of scope

Real podman's own `--all`/`--filter`, and its own multi-target
`CONTAINER [CONTAINER...]` support (with its own accompanying
"you cannot start and attach multiple containers at once" /
"you cannot start and attach all containers at once" checks) — a
separate, genuinely richer scope `ociman start` has never implemented
at all, not something this increment's own narrower `--latest`
addition needed to also solve. Continuing this same rollout
otherwise: `attach`, `port`, and `checkpoint`/`restore` (the last two
CRIU-based, a much larger, separately-scoped gap) still don't have
`--latest` here at all — each a natural, separate future increment.
