# Design note 0502: `ociman container export` alias

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_container.rs`.

## What this closes

Continuing the `ociman container` alias family `0357`/`0431`/`0474`/
`0488`-`0501` started: `export` — the sixteenth member of real
podman's own `podman container <verb>` family closed so far — was
still missing.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/export.go:22-68`:
  `containerExportCommand` (`Parent: containerCmd`) and top-level
  `exportCommand` share the exact same `Use`/`Short`/`Long`/`Args`/
  `RunE`/`ValidArgsFunction`, and both get the identical flag set
  applied via the one shared `exportFlags(cmd)` helper (`--output`/
  `-o`) — a byte-identical alias, the same shape `0492` already
  established for `Self::Kill`.

## Implementation

`ContainerCommand::Export` is a new variant, field-for-field
identical to the already-existing `Command::Export` (`id`, `output`),
dispatching into the exact same `cmd_export` function `ociman
export` itself already calls with the identical argument order —
zero new business logic, zero new primitive, the same "raw fields
straight through" shape `0489`/`0490`/`0492`/`0493`/`0494`/`0500`/
`0501` already used.

## Tests

One new integration test added to `tests/tests/ociman_container.rs`:

- `container_export_is_a_byte_identical_alias_for_top_level_export`
  — proves the alias actually writes a real, complete archive of a
  container's own filesystem, exactly like the top-level command
  (using the same `.rootless-overlay-supported` = `false` fixture
  setup `ociman_cp.rs`/`ociman_diff.rs`/`ociman_commit.rs`'s own
  tests already establish, since `export` shares the same rootless-
  overlay rootfs gap those commands do).

Full `export` semantics (stdout-by-default, a still-running
container's own live mounts correctly excluded) are already
exhaustively tested against the top-level command in
`ociman_export.rs` — this note's own test deliberately only proves
the alias itself reaches the identical function with the identical
fields, not re-testing `export`'s own semantics a second time.

All 28 tests in `tests/tests/ociman_container.rs` pass (27 prior + 1
new).

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (122
test-result blocks, 0 failures — no new test file added, so the block
count is unchanged from `0501`; clean on the first attempt with
`RUST_TEST_THREADS=2`), `python3 ci/guards.py` (clean), `cargo deny
check` (clean), `bash ci/native-ci.sh` (clean on the first attempt,
also with `RUST_TEST_THREADS=2`), `bash ci/build-deb.sh` (clean on
the first attempt, real `dpkg -i`/`--version`/`dpkg -r` round trip).
No benchmark re-run needed: `ociman container export` is not
exercised by `ci/bench.sh`, and this is a pure dispatch-reuse
addition touching no existing function's body at all.

## Deliberately still out of scope

- The rest of real podman's own `podman container <verb>` family not
  yet ported as aliases: `run`, `create`, `exec`, `attach`, `port`,
  `mount`/`unmount`, `init`, `stats`, `runlabel` — each a pure-alias
  candidate of the same shape as this one and `0488`-`0501`, left for
  future increments to keep each one individually small and
  independently verified.
</content>
