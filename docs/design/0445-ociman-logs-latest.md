# Design note 0445: `ociman logs --latest`/`-l`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_logs.rs`.

## What this closes

`ociman logs` had no `--latest`/`-l` flag at all — continuing the
same rollout `0434`-`0437`/`0443`/`0444` already established across
`rm`/`stop`/`restart`/`pause`/`unpause`/`exec`/`kill`.

## Real, checked-directly confirmation

`~/git/podman/cmd/podman/containers/logs.go:88`: `validate.
AddLatestFlag(logsCommand, &logsOptions.Latest)` — the exact same
flag/validation `ociman rm --latest` (`0434`) already ports. Its own
`Args` validator (lines ~38-48) has a real, checked-directly
different shape from every other sibling in this rollout so far,
worth citing verbatim:

```go
switch {
case logsOptions.Latest && len(args) > 0:
    return errors.New("--latest and containers cannot be used together")
case !logsOptions.Latest && len(args) < 1:
    return errors.New("specify at least one container name or ID to log")
}
```

Real podman's own `logs` actually accepts *multiple* containers
(`CONTAINER [CONTAINER...]`) — a separate, pre-existing, already-
documented scope gap this project has never matched at all (`ociman
logs` only ever took one), left completely untouched here; `--latest`
itself only ever needs the identical single-target
`resolve_latest_container` every other sibling in this rollout
already reuses.

## Implementation

- `Command::Logs::id` widens from `String` to `Option<String>`
  (omittable when using `--latest`, matching the same widening
  `0434`'s own `Command::Rm::ids` needed, scaled down to this
  command's own already-single-target shape); new `latest: bool`
  (`#[arg(short = 'l', long)]`).
- The dispatch arm for `Command::Logs` performs the exact two-case
  validation above, in real podman's own exact wording, then resolves
  either via `resolve_latest_container` or the given `id`, before
  calling `cmd_logs` with a single, already-resolved id string exactly
  as before — `cmd_logs`'s own signature/implementation is completely
  unchanged.

## Tests

Four new tests in `tests/tests/ociman_logs.rs`: `logs_latest_shows_
the_most_recently_created_containers_own_log` (two containers with a
real, distinguishable creation-time gap, each echoing genuinely
different output; `logs --latest` shows only the newer one's own
output, never the older one's — a real, convincing proof, not just
"some log was shown"), `logs_latest_and_explicit_id_together_is_a_
clear_error`, `logs_with_neither_latest_nor_an_explicit_id_is_a_
clear_error`, and `logs_latest_on_an_empty_store_is_a_clear_error`.
All 7 prior tests in the file pass unmodified (11/11 total).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (120 test-result
blocks, 0 failures, clean on the second full run — the first hit the
pre-existing, previously-documented `ocicri_container.rs` host-
contention flakiness from the long-running runaway CPU-spinning
process on this host, confirmed unrelated and transient by an
immediate isolated rerun), `python3 ci/guards.py`, `cargo deny check`,
`bash ci/native-ci.sh` (clean, 120/120, clean on the first run),
`bash ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg -r` round
trip). Touches only `ociman logs`'s own selection logic, not any hot
path at all (`ociman logs` isn't part of `ci/bench.sh`'s own
startup-/destroy-time measurements) — no benchmark re-run needed.

## Deliberately still out of scope

Continuing this same rollout: `attach`, `diff`, `inspect`, `top`,
`stats`, `wait`, `start`, `port`, and `checkpoint`/`restore` (the last
two CRIU-based, a much larger, separately-scoped gap) still don't
have `--latest` here at all — each a natural, separate future
increment.
