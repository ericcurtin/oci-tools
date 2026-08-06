# Design note 0507: `ociman container create` alias

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_container.rs`.

## What this closes

Continuing the `ociman container` alias family `0357`/`0431`/`0474`/
`0488`-`0506` started: `create` — the twenty-first member of real
podman's own `podman container <verb>` family closed so far — was
still missing.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/create.go:32-101`:
  `containerCreateCommand` (`Parent: containerCmd`) and top-level
  `createCommand` share the exact same `Args` (`cobra.MinimumNArgs
  (1)`)/`Use`/`Short`/`Long`/`RunE`/`ValidArgsFunction`, and both get
  the identical flag set applied via the one shared `createFlags
  (cmd)` helper — a byte-identical alias, the same shape `0492`
  already established for `Self::Kill`, and the same shape `0506`
  already established for `Self::Run`.

## Implementation

`ContainerCommand::Create` is a new variant, mirroring the already-
existing top-level `Command::Create`'s own shape exactly: the same
flattened `RunArgs` (boxed here, for the identical `clippy::
large_enum_variant` reason `0506`'s `Self::Run::args` doc comment
already explains) plus the same two extra fields (`rm`,
`interactive` — no `detach`/`preserve_fds`, since `create` itself
never launches anything at all, exactly matching `Command::Create`'s
own already-narrower-than-`Run` scope), dispatching into the exact
same `cmd_create` function `ociman create` itself already calls with
the identical argument order — zero new business logic, zero new
primitive.

## Tests

One new integration test added to `tests/tests/ociman_container.rs`:

- `container_create_is_a_byte_identical_alias_for_top_level_create`
  — proves the alias actually creates a real container left in a
  real `created` state, hidden from a plain `ps` but visible with
  `ps -a`, exactly like the top-level command.

Full `create` semantics (the entire `RunArgs` flag surface, `--rm`
persisted for a later `start` to honor, `--name` resolution,
`--cidfile`) are already exhaustively tested against the top-level
command in `ociman_create.rs` — this note's own test deliberately
only proves the alias itself reaches the identical function with the
identical fields, not re-testing `create`'s own semantics a second
time.

All 33 tests in `tests/tests/ociman_container.rs` pass (32 prior + 1
new).

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean — no `large_enum_variant` issue this time,
since `RunArgs` was already boxed following `0506`'s own established
pattern), `cargo test --workspace --locked` (122 test-result blocks,
0 failures — no new test file added, so the block count is unchanged
from `0506`; needed one retry — a transient `ocicri_container.rs`
flake in the same already-documented class, independently confirmed
passing in isolation, then clean with `RUST_TEST_THREADS=2`),
`python3 ci/guards.py` (clean), `cargo deny check` (clean), `bash ci/
native-ci.sh` (clean on the first attempt, also with
`RUST_TEST_THREADS=2`), `bash ci/build-deb.sh` (clean on the first
attempt, real `dpkg -i`/`--version`/`dpkg -r` round trip). No
benchmark re-run needed: this change is a pure CLI-surface addition
(a second entry point that dispatches into the exact same, entirely
unchanged `cmd_create` function) touching no existing function's body
at all — the actual hot-path spec construction the `create`+`start`
combination exercises is byte-for-byte identical to before this
change.

## Deliberately still out of scope

- The rest of real podman's own `podman container <verb>` family not
  yet ported as aliases: `port`, `mount`/`unmount`, `init`,
  `runlabel` — each a pure-alias candidate of the same shape as this
  one and `0488`-`0506`, left for future increments to keep each one
  individually small and independently verified.
</content>
