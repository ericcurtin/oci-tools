# Design note 0501: `ociman container commit` alias

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_container.rs`.

## What this closes

Continuing the `ociman container` alias family `0357`/`0431`/`0474`/
`0488`-`0500` started: `commit` — the fifteenth member of real
podman's own `podman container <verb>` family closed so far — was
still missing.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/commit.go:19-98`:
  `containerCommitCommand` (`Parent: containerCmd`) and top-level
  `commitCommand` share the exact same `Use`/`Short`/`Long`/`Args`/
  `RunE`/`ValidArgsFunction`, and both get the identical flag set
  applied via the one shared `commitFlags(cmd)` helper (`--change`/
  `-c`, `--config`, `--format`/`-f`, `--iidfile`, `--message`/`-m`,
  `--author`/`-a`, `--pause`/`-p`, `--quiet`/`-q`, `--squash`/`-s`,
  `--include-volumes`) — a byte-identical alias, the same shape
  `0492` already established for `Self::Kill`.

## Implementation

`ContainerCommand::Commit` is a new variant, field-for-field
identical to the already-existing `Command::Commit` (`container`,
`image`, `author`, `message`, `pause`, `change`, `squash`, `iidfile`),
dispatching into the exact same `cmd_commit` function `ociman commit`
itself already calls with the identical argument order (plus the
same `cli.global.json` this project's own top-level arm already
passes through) — zero new business logic, zero new primitive, the
same "raw fields straight through" shape `0489`/`0490`/`0492`/`0493`/
`0494`/`0500` already used.

This project's own top-level `commit` has always been an honestly
narrower first slice than real podman's own richer one (no
`--config`/`--format`/`--quiet`/`--include-volumes`, see
`Command::Commit`'s own doc comment), so the alias mirrors that same,
already-existing scope exactly rather than inventing a wider one that
doesn't exist at the top level either — the same pattern `0491`'s
`Start`/`0494`'s `Restart`/`0498`'s `Logs` variants already
established.

## Tests

One new integration test added to `tests/tests/ociman_container.rs`:

- `container_commit_is_a_byte_identical_alias_for_top_level_commit` —
  proves the alias actually creates a real, runnable image from a
  real container's own changes (an added file, `--author` recorded)
  and tags it, exactly like the top-level command (using the same
  `.rootless-overlay-supported` = `false` fixture setup `ociman_
  commit.rs`'s own tests already establish, since `commit` doesn't
  support this project's own rootless-overlay rootfs optimization
  yet).

Full `commit` semantics (`--message`, `--pause`/`--pause=false`,
`--change`, `--squash`, `--iidfile`, the untagged-commit case, the
rootless-overlay gap itself) are already exhaustively tested against
the top-level command in `ociman_commit.rs` — this note's own test
deliberately only proves the alias itself reaches the identical
function with the identical fields, not re-testing `commit`'s own
semantics a second time.

All 27 tests in `tests/tests/ociman_container.rs` pass (26 prior + 1
new).

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (122
test-result blocks, 0 failures — no new test file added, so the block
count is unchanged from `0500`; clean on the first attempt with
`RUST_TEST_THREADS=2`), `python3 ci/guards.py` (clean), `cargo deny
check` (clean), `bash ci/native-ci.sh` (one transient `ocicri_
container.rs` flake in the same already-documented class,
independently confirmed passing in isolation, then clean on the
second attempt with `RUST_TEST_THREADS=2`), `bash ci/build-deb.sh`
(clean on the first attempt, real `dpkg -i`/`--version`/`dpkg -r`
round trip). No benchmark re-run needed: `ociman container commit`
is not exercised by `ci/bench.sh`, and this is a pure dispatch-reuse
addition touching no existing function's body at all.

## Deliberately still out of scope

- The rest of real podman's own `podman container <verb>` family not
  yet ported as aliases: `run`, `create`, `exec`, `attach`, `export`,
  `port`, `mount`/`unmount`, `init`, `stats`, `runlabel` — each a
  pure-alias candidate of the same shape as this one and `0488`-
  `0500`, left for future increments to keep each one individually
  small and independently verified.
- Real podman's own richer `podman commit`/`podman container commit`
  (`--config`/`--format`/`--quiet`/`--include-volumes`) — a genuinely
  separate, still-open gap in the *top-level* `ociman commit` itself
  (not something this alias increment introduces or could close on
  its own), left for its own future increment.
</content>
