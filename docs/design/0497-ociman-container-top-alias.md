# Design note 0497: `ociman container top` alias

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_container.rs`.

## What this closes

Continuing the `ociman container` alias family `0357`/`0431`/`0474`/
`0488`-`0496` started: `top` — the eleventh member of real podman's
own `podman container <verb>` family closed so far — was still
missing.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/top.go:26-46`:
  `containerTopCommand` (`Parent: containerCmd`) and top-level
  `topCommand` share the exact same `Use`/`Short`/`Long`/`RunE`/
  `ValidArgsFunction` (`top` has no `Args` override either, like
  `wait` — plain `cobra.ArbitraryArgs`), and both get the identical
  `topFlags(cmd.Flags())` (the hidden, bash-completion-only
  `--list-descriptors`) plus `validate.AddLatestFlag` — a
  byte-identical alias, the same shape `0492` already established for
  `Self::Kill`.

## Implementation

`ContainerCommand::Top` is a new variant, field-for-field identical
to the already-existing `Command::Top` (`positional`, `latest`).
Real podman's own hidden `--list-descriptors` (bash-completion-only
plumbing) is deliberately not ported, matching this project's
already-established convention of skipping internal/hidden flags
with no equivalent concept here.

Since `Command::Top`'s own dispatch arm does its manual container-
reference-vs-`ps`-args disambiguation inline (there's no dedicated
`cmd_top`-adjacent wrapper that already takes a raw, unresolved
`positional`/`latest` pair the way `cmd_rm`/`cmd_stop`/etc. do), the
new arm replays the identical logic verbatim before calling the same
`cmd_top(&id, &ps_args)` — the same "replay the top-level arm's own
inline validation" shape `0488`/`0491`/`0496` (`Inspect`/`Start`/
`Wait`) already used.

## Tests

One new integration test added to `tests/tests/ociman_container.rs`:

- `container_top_is_a_byte_identical_alias_for_top_level_top` —
  proves the alias actually lists a real, running container's own
  processes (a real `ps(1)`-style header plus the container's own
  actual command), exactly like the top-level command.

Full `top` semantics (extra `ps(1)` arguments passed straight
through, `--latest`, refusing a stopped container) are already
exhaustively tested against the top-level command in
`ociman_top.rs` — this note's own test deliberately only proves the
alias itself reaches the identical function with the identical
fields, not re-testing `top`'s own semantics a second time.

All 23 tests in `tests/tests/ociman_container.rs` pass (22 prior + 1
new).

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (122
test-result blocks, 0 failures — no new test file added, so the block
count is unchanged from `0496`; needed one retry — two transient
`ocicri_container.rs` flakes in the same already-documented class,
both independently confirmed passing in isolation, then clean with
`RUST_TEST_THREADS=2`; `ps aux` again confirmed the same second,
independent, concurrent `opencode` agent process running the
identical prompt against this same checkout, the recurring
environmental factor first flagged in `0492` — `git status`/`git
fetch` both reconfirmed no actual concurrent modification of tracked
files occurred, only CPU contention), `python3 ci/guards.py` (clean),
`cargo deny check` (clean), `bash ci/native-ci.sh` (clean on the
first attempt with `RUST_TEST_THREADS=2`), `bash ci/build-deb.sh`
(clean on the first attempt, real `dpkg -i`/`--version`/`dpkg -r`
round trip). No benchmark re-run needed: `ociman container top` is
not exercised by `ci/bench.sh`, and this is a pure dispatch-reuse
addition touching no existing function's body at all.

## Deliberately still out of scope

- The rest of real podman's own `podman container <verb>` family not
  yet ported as aliases: `logs`, `cp`, `diff`, `commit`, `run`,
  `create`, `exec`, `attach`, `export`, `port`, `mount`/`unmount`,
  `init`, `stats`, `runlabel` — each a pure-alias candidate of the
  same shape as this one and `0488`-`0496`, left for future
  increments to keep each one individually small and independently
  verified.
</content>
