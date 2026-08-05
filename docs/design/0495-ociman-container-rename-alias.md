# Design note 0495: `ociman container rename` alias

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_container.rs`.

## What this closes

Continuing the `ociman container` alias family `0357`/`0431`/`0474`/
`0488`-`0494` started: `rename` — the ninth member of real podman's
own `podman container <verb>` family closed so far, and the simplest
one yet — was still missing.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/rename.go:11-41`:
  `containerRenameCommand` (`Parent: containerCmd`) and top-level
  `renameCommand` share the exact same `Use`/`Short`/`Long`/`RunE`/
  `Args`/`ValidArgsFunction` verbatim — no flags at all on either
  side, `Args: cobra.ExactArgs(2)` (`CONTAINER NAME`) — the simplest
  byte-identical alias in the whole family so far.

## Implementation

`ContainerCommand::Rename` is a new variant, field-for-field
identical to the already-existing `Command::Rename` (`id`, `name`),
dispatching into the exact same `cmd_rename` function `ociman rename`
itself already calls with the identical argument order — zero new
business logic, zero new primitive, no flags to mirror at all.

## Tests

One new integration test added to `tests/tests/ociman_container.rs`:

- `container_rename_is_a_byte_identical_alias_for_top_level_rename` —
  proves the alias actually renames a real container (old name no
  longer resolves, new name immediately usable for `rm`), and prints
  nothing on success, exactly like the top-level command.

(Initially wrote the test using `create` rather than `run` to set up
the fixture container, hitting the same "a `Created` container isn't
removable without `--force`" refusal several earlier increments in
this series already ran into — caught immediately by the test
failure, fixed by switching to `run` before ever proceeding, matching
`ociman_rename.rs`'s own established convention.)

Full `rename` semantics (name validation, collision refusal,
renaming a container to its own current name, resolving by real ID
too) are already exhaustively tested against the top-level command in
`ociman_rename.rs` — this note's own test deliberately only proves
the alias itself reaches the identical function with the identical
fields, not re-testing `rename`'s own semantics a second time.

All 21 tests in `tests/tests/ociman_container.rs` pass (20 prior + 1
new).

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (122
test-result blocks, 0 failures — no new test file added, so the block
count is unchanged from `0494`; clean on the first attempt with
`RUST_TEST_THREADS=2`, run preemptively given the same unusually
heavy concurrent load flagged in `0492`-`0494`), `python3 ci/
guards.py` (clean), `cargo deny check` (clean), `bash ci/
native-ci.sh` (clean on the first attempt, also with
`RUST_TEST_THREADS=2`), `bash ci/build-deb.sh` (clean, real `dpkg -i`/
`--version`/`dpkg -r` round trip). No benchmark re-run needed:
`ociman container rename` is not exercised by `ci/bench.sh`, and this
is a pure dispatch-reuse addition touching no existing function's
body at all.

## Deliberately still out of scope

- The rest of real podman's own `podman container <verb>` family not
  yet ported as aliases: `top`, `logs`, `cp`, `diff`, `commit`,
  `wait`, `run`, `create`, `exec`, `attach`, `export`, `port`,
  `mount`/`unmount`, `init`, `stats`, `runlabel` — each a pure-alias
  candidate of the same shape as this one and `0488`-`0494`, left for
  future increments to keep each one individually small and
  independently verified.
</content>
