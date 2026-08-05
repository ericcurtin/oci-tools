# Design note 0496: `ociman container wait` alias

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_container.rs`.

## What this closes

Continuing the `ociman container` alias family `0357`/`0431`/`0474`/
`0488`-`0495` started: `wait` — the tenth member of real podman's own
`podman container <verb>` family closed so far — was still missing.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/wait.go:20-73`:
  `containerWaitCommand` (`Parent: containerCmd`) and top-level
  `waitCommand` share the exact same `Use`/`Short`/`Long`/`RunE`/
  `ValidArgsFunction` (`wait` has no `Args` override at all, unlike
  `rm`/`stop`/`kill`/`restart` — plain positional args, checked
  directly), and both get the identical flag set applied via the one
  shared `waitFlags(cmd)` helper (`--interval`/`-i`, `--ignore`,
  `--condition`, and the not-yet-ported `--exit-first-match`) followed
  by the identical `validate.AddLatestFlag` call — a byte-identical
  alias, the same shape `0492` already established for `Self::Kill`.

## Implementation

`ContainerCommand::Wait` is a new variant, field-for-field identical
to the already-existing `Command::Wait` (`ids`, `latest`, `interval`,
`condition`, `ignore`). Since `Command::Wait`'s own dispatch arm does
its `--latest`/explicit-ids validation and resolution inline (there's
no dedicated `cmd_wait`-adjacent wrapper that already takes raw,
unresolved `ids`/`latest` fields the way `cmd_rm`/`cmd_stop`/
`cmd_kill`/`cmd_pause`/`cmd_unpause`/`cmd_restart` do), the new arm
replays the identical two checks verbatim before calling the same
`cmd_wait(&ids, interval, &condition, ignore)` — the same "replay the
top-level arm's own inline validation" shape `0488`/`0491` (`Inspect`/
`Start`) already used.

(Real podman's own `--exit-first-match` flag is not ported: this
project's own top-level `Command::Wait` has never implemented it
either — the same honestly narrower first-slice scope already
established for `restart --running`/`start --all`/etc.)

## Tests

One new integration test added to `tests/tests/ociman_container.rs`:

- `container_wait_is_a_byte_identical_alias_for_top_level_wait` —
  proves the alias actually blocks on (here, immediately returns for)
  a real container and prints its real exit code, exactly like the
  top-level command.

Full `wait` semantics (multi-id, blocking until a still-running
container actually exits, `--condition`, `--ignore`, `--latest`,
`--interval`) are already exhaustively tested against the top-level
command in `ociman_wait.rs` — this note's own test deliberately only
proves the alias itself reaches the identical function with the
identical fields, not re-testing `wait`'s own semantics a second
time.

All 22 tests in `tests/tests/ociman_container.rs` pass (21 prior + 1
new).

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (122
test-result blocks, 0 failures — no new test file added, so the block
count is unchanged from `0495`; needed one retry — a transient
`ocicri_container.rs` flake in the same already-documented class,
independently confirmed passing in isolation, then clean with
`RUST_TEST_THREADS=2`; `ps aux` again confirmed a second, genuinely
concurrent, independent `opencode` agent process running the identical
prompt against this same checkout, the same environmental factor
`0492` first flagged — `git status`/`git fetch` both reconfirmed no
actual concurrent modification of tracked files occurred, only CPU
contention), `python3 ci/guards.py` (clean), `cargo deny check`
(clean), `bash ci/native-ci.sh` (needed two retries this time under
the same load, each individual flaky failure independently confirmed
passing in isolation before retrying, clean on the third attempt with
`RUST_TEST_THREADS=2`), `bash ci/build-deb.sh` (clean on the first
attempt, real `dpkg -i`/`--version`/`dpkg -r` round trip). No
benchmark re-run needed: `ociman container wait` is not exercised by
`ci/bench.sh`, and this is a pure dispatch-reuse addition touching no
existing function's body at all.

## Deliberately still out of scope

- The rest of real podman's own `podman container <verb>` family not
  yet ported as aliases: `top`, `logs`, `cp`, `diff`, `commit`, `run`,
  `create`, `exec`, `attach`, `export`, `port`, `mount`/`unmount`,
  `init`, `stats`, `runlabel` — each a pure-alias candidate of the
  same shape as this one and `0488`-`0495`, left for future increments
  to keep each one individually small and independently verified.
</content>
