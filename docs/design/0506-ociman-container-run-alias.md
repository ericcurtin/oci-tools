# Design note 0506: `ociman container run` alias

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_container.rs`.

## What this closes

Continuing the `ociman container` alias family `0357`/`0431`/`0474`/
`0488`-`0505` started: `run` — the twentieth member of real podman's
own `podman container <verb>` family closed so far, and the richest
flag surface in the whole project (the entire `RunArgs` struct) —
was still missing.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/run.go:24-105`:
  `containerRunCommand` (`Parent: containerCmd`) and top-level
  `runCommand` share the exact same `Args` (`cobra.MinimumNArgs(1)`)/
  `Use`/`Short`/`Long`/`RunE`/`ValidArgsFunction`, and both get the
  identical flag set applied via the one shared `runFlags(cmd)`
  helper (which itself calls `common.DefineCreateFlags`/
  `common.DefineNetFlags`, the same shared flag-registration real
  podman's own `create` also uses) — a byte-identical alias, the
  same shape `0492` already established for `Self::Kill`.

## Implementation

`ContainerCommand::Run` is a new variant. This project's own top-level
`Command::Run` already flattens its own ~60-field `RunArgs` struct via
`#[command(flatten)] args: RunArgs`, plus four of its own extra fields
(`rm`, `detach`, `interactive`, `preserve_fds`) — the alias mirrors
that identical shape exactly (`#[command(flatten)] args: Box<RunArgs>`
plus the same four fields), dispatching into the exact same `cmd_run`
function `ociman run` itself already calls with the identical argument
order — zero new business logic, zero new primitive.

`RunArgs` is boxed here (unlike the top-level `Command::Run::args`
field, which isn't) purely to keep this smaller, otherwise-lightweight
`ContainerCommand` enum's own overall size down: `clippy::
large_enum_variant` correctly flagged that embedding the ~1100-byte
`RunArgs` directly would make this one variant dominate the whole
enum's size, since every other `ContainerCommand` variant is tiny —
`clap` flattens through a `Box` exactly the same way, with zero
behavior difference.

## Tests

One new integration test added to `tests/tests/ociman_container.rs`:

- `container_run_is_a_byte_identical_alias_for_top_level_run` —
  proves the alias actually runs a real container, that explicit
  command-line arguments override the image's own declared default
  command, and that `--rm` reclaims the container's storage
  afterward, exactly like the top-level command.

Full `run` semantics (the entire, enormous `RunArgs` flag surface —
volumes, networking-adjacent flags, resource limits, security
options, environment, labels, health checks, and everything else
`ociman run`/`ociman create` already share) are already exhaustively
tested against the top-level command across `ociman_run.rs` and many
other dedicated test files — this note's own test deliberately only
proves the alias itself reaches the identical function with the
identical fields, not re-testing `run`'s own semantics a second time.

All 32 tests in `tests/tests/ociman_container.rs` pass (31 prior + 1
new).

Full workspace: `cargo build --workspace --locked` (clean — required
boxing `RunArgs` to satisfy `clippy::large_enum_variant`), `cargo fmt
--all` (clean), `cargo clippy --workspace --all-targets --locked --
-D warnings` (clean), `cargo test --workspace --locked` (122
test-result blocks, 0 failures — no new test file added, so the block
count is unchanged from `0505`; clean on the first attempt with
`RUST_TEST_THREADS=2`), `python3 ci/guards.py` (clean), `cargo deny
check` (clean), `bash ci/native-ci.sh` (one transient
`ocicri_container.rs` flake in the same already-documented class on
the first attempt, independently confirmed passing in isolation,
then clean on the second attempt with `RUST_TEST_THREADS=2`), `bash
ci/build-deb.sh` (clean on the first attempt, real `dpkg -i`/
`--version`/`dpkg -r` round trip). No benchmark re-run needed: this
change is a pure CLI-surface addition (a second entry point that
dispatches into the exact same, entirely unchanged `cmd_run`
function) touching no existing function's body at all — the actual
hot-path spec construction/launch mechanism `ci/bench.sh` exercises
is byte-for-byte identical to before this change, only reachable
through one more subcommand path now.

## Deliberately still out of scope

- The rest of real podman's own `podman container <verb>` family not
  yet ported as aliases: `create`, `port`, `mount`/`unmount`, `init`,
  `runlabel` — each a pure-alias candidate of the same shape as this
  one and `0488`-`0505` (`create` in particular should be nearly
  identical in shape to this increment, sharing the same flattened
  `RunArgs`), left for future increments to keep each one
  individually small and independently verified.
</content>
