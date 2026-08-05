# Design note 0492: `ociman container kill` alias

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_container.rs`.

## What this closes

Continuing the `ociman container` alias family `0357`/`0431`/`0474`/
`0488`/`0489`/`0490`/`0491` started: `kill` — the fifth member of real
podman's own `podman container <verb>` family closed so far — was
still missing.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/kill.go:20-46`:
  `containerKillCommand` (`Parent: containerCmd`) and top-level
  `killCommand` share the exact same `Use`/`Short`/`Long`/`RunE`/
  `Args`/`ValidArgsFunction`, and both get the identical flag set
  applied via the one shared `killFlags(cmd)` helper (`--all`/`-a`,
  `--signal`/`-s` default `KILL`, `--cidfile`) followed by the
  identical `validate.AddLatestFlag` call — a byte-identical alias,
  the same shape `0491` already established for `Self::Start`.

## Implementation

`ContainerCommand::Kill` is a new variant, field-for-field identical
to the already-existing `Command::Kill` (`ids`, `signal`, `all`,
`cidfile`, `latest`), dispatching into the exact same `cmd_kill`
function `ociman kill` itself already calls with the identical
argument order — zero new business logic, zero new primitive, the
same "raw fields straight through" shape `0489`/`0490` (`Rm`/`Stop`)
already used, not the "replay inline validation" shape `0488`/`0491`
(`Inspect`/`Start`) needed.

## Tests

Two new integration tests added to `tests/tests/ociman_container.rs`:

- `container_kill_is_a_byte_identical_alias_for_top_level_kill` —
  proves the alias actually signals a real, running container with
  the default `KILL` and prints its id, exactly like the top-level
  command.
- `container_kill_signal_flag_works_through_the_alias` — proves the
  alias's own flag set works too: `--signal TERM` sends exactly that
  signal with no escalation, matching the top-level `ociman kill
  --signal`'s own already-established behavior exactly.

Full `kill` semantics (multi-id resolve-then-act, `--all`,
`--cidfile`, `--latest`) are already exhaustively tested against the
top-level command in `ociman_kill.rs` — this note's own tests
deliberately only prove the alias itself reaches the identical
function with the identical fields, not re-testing `kill`'s own
semantics a second time.

All 18 tests in `tests/tests/ociman_container.rs` pass (16 prior + 2
new).

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (122
test-result blocks, 0 failures — no new test file added, so the block
count is unchanged from `0491`; needed three retries under unusually
heavy concurrent load this session — the same already-documented
long-running CPU-spinning background process plus, confirmed directly
via `ps aux` this time, a second, genuinely concurrent, independent
`opencode` agent process running the identical prompt against this
same checkout; each individual flaky failure across `ocicri_
container.rs`/`ociman_logs.rs`/`ociman_exec.rs` was independently
confirmed passing instantly in isolation before retrying, and `git
status`/`git fetch` were both checked to confirm no actual concurrent
modification of this repository's own tracked files occurred despite
the second agent process, only extra CPU contention; the final,
successful attempt used `RUST_TEST_THREADS=2`), `python3 ci/
guards.py` (clean), `cargo deny check` (clean), `bash ci/
native-ci.sh` (also needed `RUST_TEST_THREADS=2` on its own fourth
attempt, clean once given it, same root cause), `bash ci/build-deb.sh`
(clean on the first attempt, real `dpkg -i`/`--version`/`dpkg -r`
round trip). No benchmark re-run needed: `ociman container kill` is
not exercised by `ci/bench.sh`, and this is a pure dispatch-reuse
addition touching no existing function's body at all.

## Deliberately still out of scope

- The rest of real podman's own `podman container <verb>` family not
  yet ported as aliases: `top`, `logs`, `cp`, `diff`, `commit`,
  `pause`/`unpause`, `rename`, `restart`, `wait`, `run`, `create`,
  `exec`, `attach`, `export`, `port`, `mount`/`unmount`, `init`,
  `stats`, `runlabel` — each a pure-alias candidate of the same shape
  as this one and `0488`-`0491`, left for future increments to keep
  each one individually small and independently verified.
</content>
