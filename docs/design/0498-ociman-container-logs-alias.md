# Design note 0498: `ociman container logs` alias

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_container.rs`.

## What this closes

Continuing the `ociman container` alias family `0357`/`0431`/`0474`/
`0488`-`0497` started: `logs` — the twelfth member of real podman's
own `podman container <verb>` family closed so far — was still
missing.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/logs.go:34-73`:
  `containerLogsCommand` (`Parent: containerCmd`) and top-level
  `logsCommand` share the exact same `Use`/`Short`/`Long`/`Args`/
  `RunE`/`ValidArgsFunction`, and both get the identical flag set
  applied via the one shared `logsFlags(cmd)` helper (`--follow`/
  `-f`, `--since`, `--until`, `--tail`, `--timestamps`/`-t`,
  `--color`, `--names`/`-n`, and the hidden `--details`) followed by
  the identical `validate.AddLatestFlag` call — a byte-identical
  alias, the same shape `0492` already established for `Self::Kill`.

## Implementation

`ContainerCommand::Logs` is a new variant, field-for-field identical
to the already-existing `Command::Logs` (`id`, `latest`, `follow`,
`tail`) — this project's own top-level `logs` has always been an
honestly narrower first slice than real podman's own richer one (no
`--since`/`--until`/`--timestamps`/`--color`/`--names`/`--details`,
no multi-container support), so the alias mirrors that same,
already-existing scope exactly rather than inventing a wider one that
doesn't exist at the top level either — the same pattern `0491`'s
`Start` variant already established.

Since `Command::Logs`'s own dispatch arm does its `--latest`/
explicit-id validation and resolution inline (there's no dedicated
`cmd_logs`-adjacent wrapper that already takes raw, unresolved
`id`/`latest` fields), the new arm replays the identical checks
verbatim before calling the same `cmd_logs(&resolved_id, follow,
tail)` — the same "replay the top-level arm's own inline validation"
shape `0488`/`0491`/`0496`/`0497` already used.

## Tests

One new integration test added to `tests/tests/ociman_container.rs`:

- `container_logs_is_a_byte_identical_alias_for_top_level_logs` —
  proves the alias actually shows a real, finished container's own
  combined stdout/stderr output, exactly like the top-level command.

Full `logs` semantics (`--follow` streaming/stopping on exit,
`--tail`, `--latest`, catch-up-vs-live-output trimming) are already
exhaustively tested against the top-level command in
`ociman_logs.rs` — this note's own test deliberately only proves the
alias itself reaches the identical function with the identical
fields, not re-testing `logs`'s own semantics a second time.

All 24 tests in `tests/tests/ociman_container.rs` pass (23 prior + 1
new).

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (122
test-result blocks, 0 failures — no new test file added, so the block
count is unchanged from `0497`; needed three retries this time under
unusually persistent transient flakiness — two separate
`ocicri_container.rs` failures and one `ociman_logs.rs` follow-test
failure, all in the same already-documented flaky classes, each
independently confirmed passing instantly in isolation before
retrying; system load was checked directly this time (`uptime`/
`free -h`, load average ~1.7 of 20 cores, 54Gi free memory) and found
genuinely low overall — the flakiness traces to the single, already-
documented long-running CPU-spinning background process alone (pid
678558, since Jul23) contending for one core at exactly the wrong
moment during a timing-sensitive launcher handshake, not a second
concurrent agent this time (confirmed via `ps aux`: the other visible
`opencode` processes were this same session's own outer wrapper and
unrelated, low-CPU sessions)), `python3 ci/guards.py` (clean), `cargo
deny check` (clean), `bash ci/native-ci.sh` (one transient
`ocicri_container.rs` flake on the first attempt, same class,
independently confirmed passing in isolation, then clean on the
second attempt), `bash ci/build-deb.sh` (clean on the first attempt,
real `dpkg -i`/`--version`/`dpkg -r` round trip). No benchmark re-run
needed: `ociman container logs` is not exercised by `ci/bench.sh`,
and this is a pure dispatch-reuse addition touching no existing
function's body at all.

## Deliberately still out of scope

- The rest of real podman's own `podman container <verb>` family not
  yet ported as aliases: `cp`, `diff`, `commit`, `run`, `create`,
  `exec`, `attach`, `export`, `port`, `mount`/`unmount`, `init`,
  `stats`, `runlabel` — each a pure-alias candidate of the same shape
  as this one and `0488`-`0497`, left for future increments to keep
  each one individually small and independently verified.
- Real podman's own richer `podman logs`/`podman container logs`
  (`--since`/`--until`/`--timestamps`/`--color`/`--names`/`--details`,
  multi-container support) — a genuinely separate, still-open gap in
  the *top-level* `ociman logs` itself (not something this alias
  increment introduces or could close on its own), left for its own
  future increment.
</content>
