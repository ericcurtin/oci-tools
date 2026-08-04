# Design note 0435: `ociman stop --latest`/`-l`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_stop.rs`,
`README.md`.

## What this closes

`ociman stop` had no `--latest`/`-l` flag at all. This is the second
of the five sibling commands real podman offers the identical flag
on (`rm`/`stop`/`restart`/`pause`/`unpause`), continuing the
deliberately one-command-per-note rollout `0434`'s own design note
already committed to.

## Real, checked-directly confirmation

`~/git/podman/cmd/podman/containers/stop.go:94,101`:
`validate.AddLatestFlag(stopCommand, &stopOptions.Latest)` — the
exact same flag/validation `ociman rm --latest` (`0434`) already
ports, reused verbatim (`GetLatestContainer`'s own semantics,
`validate.CheckAllLatestAndIDFile`'s own mutual-exclusivity matrix —
see `0434`'s own design note for the full citation, not repeated
here).

## Implementation

- `Command::Stop` gains `latest: bool` (`#[arg(short = 'l', long)]`).
- `cmd_stop` gains a `latest: bool` parameter and the same mutual-
  exclusivity check its `--filter`/`--all`/`--cidfile` siblings
  already have. Unlike `rm` (which handles `--latest` as its own
  early-return branch), `stop` merges the resolved single id straight
  into the same `ids: Vec<String>` its own `--cidfile` handling
  already builds — right after the cidfile merge, before the
  existing `all`/multi-id logic runs — so `--latest` needs no
  separate selection/removal logic of its own at all; it rides the
  exact same single-target path an explicit `ID` already takes.
  Reuses `resolve_latest_container` (introduced in `0434` explicitly
  as shared infrastructure for exactly this rollout) unchanged.

## Tests

Three new tests in `tests/tests/ociman_stop.rs`:
`stop_latest_acts_only_on_the_most_recently_created_container` (two
containers with a real, distinguishable creation-time gap; only the
newer one is targeted, confirming both `--latest` and `-l` work),
`stop_latest_on_an_empty_store_is_a_clear_error`, and `stop_latest_
combined_with_anything_else_is_a_clear_error` (all three real
mutual-exclusivity cases). All 22 prior tests in `ociman_stop.rs`
continue to pass unmodified (25/25 total).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean, no diff), `cargo clippy --workspace --all-targets --locked --
-D warnings`, `cargo test --workspace --locked` (119 test-result
blocks, 0 failures, clean on the second full run — one earlier
attempt hit the known, pre-existing `ocicri_container.rs` host-
contention flake, confirmed environmental via an immediate isolated
rerun), `python3 ci/guards.py`, `cargo deny check`, `bash ci/
native-ci.sh` (clean, 119/119), `bash ci/build-deb.sh` (real
`dpkg -i`/`--version`/`dpkg -r` round trip). Touches only `ociman
stop`'s own selection logic, not any hot path at all — no benchmark
re-run needed.

## Deliberately still out of scope

`ociman restart`/`pause`/`unpause --latest`/`-l` — now down to three
remaining sibling commands in this same rollout, each still a
natural, similarly small, mechanical port reusing the exact same
shared `resolve_latest_container` this and `0434` already
established.
