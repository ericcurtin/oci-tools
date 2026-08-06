# Design note 0503: `ociman container stats` alias

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_container.rs`.

## What this closes

Continuing the `ociman container` alias family `0357`/`0431`/`0474`/
`0488`-`0502` started: `stats` — the seventeenth member of real
podman's own `podman container <verb>` family closed so far — was
still missing.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/stats.go:22-93`:
  `containerStatsCommand` (`Parent: containerCmd`) and top-level
  `statsCommand` share the exact same `Use`/`Short`/`Long`/`Args`/
  `RunE`/`ValidArgsFunction`, and both get the identical flag set
  applied via the one shared `statFlags(cmd)` helper (`--all`/`-a`,
  `--format`, `--no-reset`, `--no-stream`, `--interval`) followed by
  the identical `validate.AddLatestFlag` call — a byte-identical
  alias, the same shape `0492` already established for `Self::Kill`.

## Implementation

`ContainerCommand::Stats` is a new variant, field-for-field identical
to the already-existing `Command::Stats` (`id`, `latest`,
`no_stream`, `interval`, `no_reset`, `format`) — this project's own
top-level `stats` has always been an honestly narrower first slice
than real podman's own richer one (no `--all`, single container
only, see `Command::Stats`'s own doc comment), so the alias mirrors
that same, already-existing scope exactly rather than inventing a
wider one that doesn't exist at the top level either — the same
pattern `0491`'s `Start`/`0494`'s `Restart`/`0498`'s `Logs`/`0501`'s
`Commit` variants already established.

Since `Command::Stats`'s own dispatch arm does its `--latest`/
explicit-id validation and resolution inline (there's no dedicated
`cmd_stats`-adjacent wrapper that already takes raw, unresolved
`id`/`latest` fields), the new arm replays the identical checks
verbatim before calling the same `cmd_stats(&resolved_id, no_stream,
interval, no_reset, cli.global.json, format.as_deref())` — the same
"replay the top-level arm's own inline validation" shape `0488`/
`0491`/`0496`/`0497`/`0498` already used.

## Tests

One new integration test added to `tests/tests/ociman_container.rs`:

- `container_stats_is_a_byte_identical_alias_for_top_level_stats` —
  proves the alias actually reports a real, running container's own
  resource usage (a non-zero real memory usage sample) via
  `--no-stream --json`, exactly like the top-level command.

Full `stats` semantics (continuous streaming and its own clean end
condition, `--format`, `--latest`, real CPU/memory/PID accounting)
are already exhaustively tested against the top-level command in
`ociman_stats.rs` — this note's own test deliberately only proves the
alias itself reaches the identical function with the identical
fields, not re-testing `stats`'s own semantics a second time.

All 29 tests in `tests/tests/ociman_container.rs` pass (28 prior + 1
new).

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (122
test-result blocks, 0 failures — no new test file added, so the block
count is unchanged from `0502`; clean on the first attempt with
`RUST_TEST_THREADS=2`), `python3 ci/guards.py` (clean), `cargo deny
check` (clean), `bash ci/native-ci.sh` (clean on the first attempt,
also with `RUST_TEST_THREADS=2`), `bash ci/build-deb.sh` (clean on
the first attempt, real `dpkg -i`/`--version`/`dpkg -r` round trip).
No benchmark re-run needed: `ociman container stats` is not
exercised by `ci/bench.sh`, and this is a pure dispatch-reuse
addition touching no existing function's body at all.

## Deliberately still out of scope

- The rest of real podman's own `podman container <verb>` family not
  yet ported as aliases: `run`, `create`, `exec`, `attach`, `port`,
  `mount`/`unmount`, `init`, `runlabel` — each a pure-alias candidate
  of the same shape as this one and `0488`-`0502`, left for future
  increments to keep each one individually small and independently
  verified.
- Real podman's own richer `podman stats`/`podman container stats`
  (`--all`, multi-container streaming) — a genuinely separate,
  still-open gap in the *top-level* `ociman stats` itself (not
  something this alias increment introduces or could close on its
  own), left for its own future increment.
</content>
