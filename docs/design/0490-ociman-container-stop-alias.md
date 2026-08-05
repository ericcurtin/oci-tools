# Design note 0490: `ociman container stop` alias

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_container.rs`.

## What this closes

Continuing the `ociman container` alias family `0357`/`0431`/`0474`/
`0488`/`0489` started: `stop` — the third member of real podman's own
`podman container <verb>` family closed so far — was still missing.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/stop.go:36-101`:
  `containerStopCommand` (`Parent: containerCmd`) and top-level
  `stopCommand` share the exact same `Use`/`Short`/`Long`/`RunE`/
  `Args`/`ValidArgsFunction`, and both get the identical flag set
  applied via the one shared `stopFlags(cmd)` helper (`--all`/`-a`,
  `--ignore`/`-i`, `--cidfile`, `--time`/`-t`, `--filter`/`-f`, plus
  the hidden `--service`) followed by the identical
  `validate.AddLatestFlag` call — a byte-identical alias, the same
  shape `0489` already established for `Self::Rm`.

## Implementation

`ContainerCommand::Stop` is a new variant, field-for-field identical
to the already-existing `Command::Stop` (`ids`, `time`, `signal`,
`all`, `cidfile`, `ignore`, `filter`, `latest`), dispatching into the
exact same `cmd_stop` function `ociman stop` itself already calls
with the identical argument order — zero new business logic, zero new
primitive.

(Real podman's own hidden `--service` flag is not ported: it's
explicitly internal/hidden plumbing for podman's own systemd-service
integration, `flags.MarkHidden(serviceFlagName)`, with no equivalent
concept in this project at all — the same deliberate omission this
project already applies to every other hidden/internal real-podman
flag it has no equivalent for.)

## Tests

Two new integration tests added to `tests/tests/ociman_container.rs`:

- `container_stop_is_a_byte_identical_alias_for_top_level_stop` —
  proves the alias actually stops a real, running container and
  prints its id, exactly like the top-level command.
- `container_stop_time_flag_works_through_the_alias` — proves the
  alias's own flag set works too: `--time 0` stops nearly immediately
  through the alias, matching the top-level `ociman stop --time`'s
  own already-established behavior.

Full `stop` semantics (graceful signal/escalation, `--all`,
`--cidfile`, `--ignore`, `--signal`, `--filter`, `--latest`) are
already exhaustively tested against the top-level command in
`ociman_stop.rs` — this note's own tests deliberately only prove the
alias itself reaches the identical function with the identical
fields, not re-testing `stop`'s own semantics a second time.

All 14 tests in `tests/tests/ociman_container.rs` pass (12 prior + 2
new).

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (122
test-result blocks, 0 failures — no new test file added, so the block
count is unchanged from `0489`), `python3 ci/guards.py` (clean),
`cargo deny check` (clean), `bash ci/native-ci.sh` (clean on the first
attempt), `bash ci/build-deb.sh` (clean, real `dpkg -i`/`--version`/
`dpkg -r` round trip). No benchmark re-run needed: `ociman container
stop` is not exercised by `ci/bench.sh`, and this is a pure
dispatch-reuse addition touching no existing function's body at all.

## Deliberately still out of scope

- The rest of real podman's own `podman container <verb>` family not
  yet ported as aliases: `start`, `top`, `logs`, `cp`, `diff`,
  `commit`, `kill`, `pause`/`unpause`, `rename`, `restart`, `wait`,
  `run`, `create`, `exec`, `attach`, `export`, `port`, `mount`/
  `unmount`, `init`, `stats`, `runlabel` — each a pure-alias candidate
  of the same shape as this one and `0488`/`0489`, left for future
  increments to keep each one individually small and independently
  verified.
</content>
