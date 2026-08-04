# Design note 0436: `ociman restart --latest`/`-l`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_start.rs`,
`README.md`.

## What this closes

`ociman restart` had no `--latest`/`-l` flag at all. This is the
third of the five sibling commands real podman offers the identical
flag on (`rm`/`stop`/`restart`/`pause`/`unpause`), continuing the
deliberately one-command-per-note rollout `0434`/`0435` already
committed to.

## Real, checked-directly confirmation

`~/git/podman/cmd/podman/containers/restart.go:86,93`: `validate.
AddLatestFlag(restartCommand, &restartOpts.Latest)` — the exact same
flag/validation `ociman rm --latest` (`0434`) already ports, reused
verbatim (`GetLatestContainer`'s own semantics, `validate.
CheckAllLatestAndIDFile`'s own mutual-exclusivity matrix — see
`0434`'s own design note for the full citation, not repeated here).

## Implementation

- `Command::Restart` gains `latest: bool` (`#[arg(short = 'l', long)]`).
- `cmd_restart` gains a `latest: bool` parameter and the same mutual-
  exclusivity check its `--filter`/`--all`/`--cidfile` siblings
  already have. Like `stop` (`0435`), and unlike `rm`, `restart`
  merges the resolved single id straight into the same `ids:
  Vec<String>` its own `--cidfile` handling already builds — right
  after the cidfile merge, before the existing `all` mutual-
  exclusivity check — so `--latest` needs no separate selection
  logic of its own at all; the single resolved id flows through the
  exact same single/multi-target path (`restart_one`/`restart_many`)
  an explicit `ID` already takes, automatically inheriting its own
  established deferred-scope-reset handling with no change needed.
  Reuses `resolve_latest_container` (introduced in `0434` explicitly
  as shared infrastructure for exactly this rollout) unchanged.

## Tests

Three new tests in `tests/tests/ociman_start.rs` (where this
project's own existing `restart` test suite already lives):
`restart_latest_acts_only_on_the_most_recently_created_container`
(two containers with a real, distinguishable creation-time gap; only
the newer, never-started one is started for the first time — its own
`true` command runs and exits, landing on `Stopped`, exactly matching
`restart --all`'s own already-established treatment of a never-
started container — while the earlier one stays untouched, still
`Created`), `restart_latest_on_an_empty_store_is_a_clear_error`, and
`restart_latest_combined_with_anything_else_is_a_clear_error` (all
three real mutual-exclusivity cases). All 21 prior tests in
`ociman_start.rs` continue to pass unmodified (24/24 total).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean, no diff), `cargo clippy --workspace --all-targets --locked --
-D warnings`, `cargo test --workspace --locked` (119 test-result
blocks, 0 failures, clean on the first full run), `python3 ci/
guards.py`, `cargo deny check`, `bash ci/native-ci.sh` (clean,
119/119), `bash ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg
-r` round trip). Touches only `ociman restart`'s own selection
logic, not any hot path at all — no benchmark re-run needed.

## Deliberately still out of scope

`ociman pause`/`unpause --latest`/`-l` — now down to two remaining
sibling commands in this same rollout, each still a natural,
similarly small, mechanical port reusing the exact same shared
`resolve_latest_container` this and `0434`/`0435` already
established.
