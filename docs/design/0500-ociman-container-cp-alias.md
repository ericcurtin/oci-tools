# Design note 0500: `ociman container cp` alias

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_container.rs`.

## What this closes

Continuing the `ociman container` alias family `0357`/`0431`/`0474`/
`0488`-`0499` started: `cp` — the fourteenth member of real podman's
own `podman container <verb>` family closed so far — was still
missing.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/cp.go:31-79`:
  `containerCpCommand` (`Parent: containerCmd`) and top-level
  `cpCommand` share the exact same `Use`/`Short`/`Long`/`Args`/
  `RunE`/`ValidArgsFunction`, and both get the identical flag set
  applied via the one shared `cpFlags(cmd)` helper (`--overwrite`,
  plus the hidden, deprecated-NOP `--archive`/`-a`/`--extract`/
  `--pause`) — a byte-identical alias, the same shape `0492` already
  established for `Self::Kill`.

## Implementation

`ContainerCommand::Cp` is a new variant, field-for-field identical to
the already-existing `Command::Cp` (`src`, `dest`, `overwrite`),
dispatching into the exact same `cmd_cp` function `ociman cp` itself
already calls with the identical argument order — zero new business
logic, zero new primitive, the same "raw fields straight through"
shape `0489`/`0490`/`0492`/`0493`/`0494` already used.

Real podman's own hidden, deprecated-NOP `--archive`/`-a`/`--extract`/
`--pause` flags are deliberately not ported: they're internal
backwards-compatibility plumbing with no real effect even in real
podman itself, matching this project's already-established convention
of skipping internal/hidden flags with no equivalent concept here.

## Tests

One new integration test added to `tests/tests/ociman_container.rs`:

- `container_cp_is_a_byte_identical_alias_for_top_level_cp` — proves
  the alias actually copies a real file from the host into a real
  container's own root filesystem, exactly like the top-level command
  (using the same `.rootless-overlay-supported` = `false` fixture
  setup `ociman_cp.rs`'s own tests already establish, since `cp`
  doesn't support this project's own rootless-overlay rootfs
  optimization yet).

Full `cp` semantics (both directions, directories, `--overwrite`,
copying between two containers, the rootless-overlay gap itself) are
already exhaustively tested against the top-level command in
`ociman_cp.rs` — this note's own test deliberately only proves the
alias itself reaches the identical function with the identical
fields, not re-testing `cp`'s own semantics a second time.

All 26 tests in `tests/tests/ociman_container.rs` pass (25 prior + 1
new).

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (122
test-result blocks, 0 failures — no new test file added, so the block
count is unchanged from `0499`; needed two retries under continuing
transient flakiness in the same already-documented `ocicri_
container.rs` class, each independently confirmed passing in
isolation before retrying, clean on the third attempt with
`RUST_TEST_THREADS=2`), `python3 ci/guards.py` (clean), `cargo deny
check` (clean), `bash ci/native-ci.sh` (needed two retries this time
under the same continuing flakiness, each failure independently
confirmed passing in isolation, clean on the third attempt with
`RUST_TEST_THREADS=2`), `bash ci/build-deb.sh` (clean on the first
attempt, real `dpkg -i`/`--version`/`dpkg -r` round trip). No
benchmark re-run needed: `ociman container cp` is not exercised by
`ci/bench.sh`, and this is a pure dispatch-reuse addition touching no
existing function's body at all.

## Deliberately still out of scope

- The rest of real podman's own `podman container <verb>` family not
  yet ported as aliases: `commit`, `run`, `create`, `exec`, `attach`,
  `export`, `port`, `mount`/`unmount`, `init`, `stats`, `runlabel` —
  each a pure-alias candidate of the same shape as this one and
  `0488`-`0499`, left for future increments to keep each one
  individually small and independently verified.
</content>
