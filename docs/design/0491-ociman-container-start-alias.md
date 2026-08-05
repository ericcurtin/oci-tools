# Design note 0491: `ociman container start` alias

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_container.rs`.

## What this closes

Continuing the `ociman container` alias family `0357`/`0431`/`0474`/
`0488`/`0489`/`0490` started: `start` — the fourth member of real
podman's own `podman container <verb>` family closed so far — was
still missing.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/start.go:20-39`:
  `containerStartCommand` (`Parent: containerCmd`) and top-level
  `startCommand` share the exact same `Use`/`Short`/`Long`/`RunE`/
  `Args`/`ValidArgsFunction`, and both get the identical flag set
  applied via the one shared `startFlags(cmd)` helper (`--attach`/
  `-a`, `--detach-keys`, `--interactive`/`-i`, `--sig-proxy`,
  `--filter`/`-f`, `--all`) followed by the identical
  `validate.AddLatestFlag` call — a byte-identical alias, the same
  shape `0490` already established for `Self::Stop`.

## Implementation

`ContainerCommand::Start` is a new variant, field-for-field identical
to the already-existing `Command::Start` (`id`, `latest`, `attach`) —
this project's own `start` has always been an honestly narrower first
slice than real podman's own richer one (no `--all`/`--filter`/
`--interactive`/`--detach-keys`/`--sig-proxy`, no multi-id at all,
see `Command::Start`'s own doc comment), so the alias mirrors that
same, already-existing scope exactly rather than inventing a wider
one that doesn't exist at the top level either.

Since `Command::Start`'s own dispatch arm does its `--latest`/
explicit-id validation and resolution inline (there's no dedicated
`cmd_start`-adjacent wrapper that already takes raw, unresolved
`id`/`latest` fields the way `cmd_rm`/`cmd_stop` do), the new
`ContainerCommand::Start` arm replays the identical two checks
verbatim before calling the same `cmd_start(&resolved_id, attach)` —
the same "replay the top-level arm's own inline validation" shape
`0488`'s `Inspect` variant already used, not the plain "raw fields
straight through" shape `Rm`/`Stop` used.

## Tests

Two new integration tests added to `tests/tests/ociman_container.rs`:

- `container_start_is_a_byte_identical_alias_for_top_level_start` —
  proves the alias actually starts a real, previously-`create`d
  container (`created` → `stopped` after its command exits) and
  prints its id, exactly like the top-level command.
- `container_start_latest_and_explicit_id_together_is_a_clear_error`
  — proves the alias's own replayed validation works too, matching
  the top-level `ociman start`'s own already-established error
  exactly.

Full `start` semantics (attach/stdin forwarding, latest resolution,
re-running an already-stopped container) are already exhaustively
tested against the top-level command in `ociman_start.rs` — this
note's own tests deliberately only prove the alias itself reaches the
identical function with the identical fields, not re-testing `start`'s
own semantics a second time.

All 16 tests in `tests/tests/ociman_container.rs` pass (14 prior + 2
new).

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (122
test-result blocks, 0 failures — no new test file added, so the block
count is unchanged from `0490`), `python3 ci/guards.py` (clean),
`cargo deny check` (clean), `bash ci/native-ci.sh` (clean on the first
attempt), `bash ci/build-deb.sh` (clean, real `dpkg -i`/`--version`/
`dpkg -r` round trip). No benchmark re-run needed: `ociman container
start` is not exercised by `ci/bench.sh`, and this is a pure
dispatch-reuse addition touching no existing function's body at all.

## Deliberately still out of scope

- The rest of real podman's own `podman container <verb>` family not
  yet ported as aliases: `top`, `logs`, `cp`, `diff`, `commit`,
  `kill`, `pause`/`unpause`, `rename`, `restart`, `wait`, `run`,
  `create`, `exec`, `attach`, `export`, `port`, `mount`/`unmount`,
  `init`, `stats`, `runlabel` — each a pure-alias candidate of the
  same shape as this one and `0488`/`0489`/`0490`, left for future
  increments to keep each one individually small and independently
  verified.
- Real podman's own richer `podman start`/`podman container start`
  (`--all`/`--filter`/`--interactive`/`--detach-keys`/`--sig-proxy`,
  multi-id) — a genuinely separate, still-open gap in the *top-level*
  `ociman start` itself (not something this alias increment
  introduces or could close on its own), left for its own future
  increment.
</content>
