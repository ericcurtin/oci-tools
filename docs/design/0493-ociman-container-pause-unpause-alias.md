# Design note 0493: `ociman container pause`/`unpause` aliases

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_container.rs`.

## What this closes

Continuing the `ociman container` alias family `0357`/`0431`/`0474`/
`0488`-`0492` started: `pause`/`unpause` — the sixth and seventh
members of real podman's own `podman container <verb>` family closed
so far — were still missing. Closing both together in one increment
since they're an inseparable pair sharing the exact same shape (the
same real podman source files even define `pause`/`unpause` next to
each other, and this project's own top-level `Command::Pause`/
`Command::Unpause` are already documented as a matched pair).

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/pause.go:19-49`:
  `containerPauseCommand` (`Parent: containerCmd`) and top-level
  `pauseCommand` share the exact same `Use`/`Short`/`Long`/`RunE`/
  `Args`/`ValidArgsFunction`, and both get the identical flag set
  applied via the one shared `pauseFlags(cmd)` helper (`--all`/`-a`,
  `--cidfile`, `--filter`/`-f`) followed by the identical
  `validate.AddLatestFlag` call.
- `~/git/podman/cmd/podman/containers/unpause.go:19-49`: the identical
  shape again for `containerUnpauseCommand`/`unpauseCommand` and
  `unpauseFlags(cmd)`.
- Both are a byte-identical alias, the same shape `0492` already
  established for `Self::Kill`.

## Implementation

`ContainerCommand::Pause`/`ContainerCommand::Unpause` are two new
variants, field-for-field identical to the already-existing
`Command::Pause`/`Command::Unpause` (`ids`, `all`, `cidfile`,
`filter`, `latest`), dispatching into the exact same `cmd_pause`/
`cmd_unpause` functions `ociman pause`/`ociman unpause` themselves
already call with the identical argument order — zero new business
logic, zero new primitive, the same "raw fields straight through"
shape `0489`/`0490`/`0492` already used.

## Tests

One new integration test added to `tests/tests/ociman_container.rs`,
covering both aliases together in a single real pause/unpause round
trip (mirroring how the top-level commands are already tested as an
inseparable pair in `ociman_pause.rs`):

- `container_pause_and_unpause_are_byte_identical_aliases_for_top_level_pause_and_unpause`
  — proves the `pause` alias actually freezes a real, running
  container (`running` → `paused`, real cgroup-v2 freezer state) and
  prints its id, then proves the `unpause` alias actually thaws it
  back (`paused` → `running`) and prints its id too, exactly like the
  top-level commands.

Full `pause`/`unpause` semantics (`--all`, `--cidfile`, `--filter`,
`--latest`, every real state-transition edge case) are already
exhaustively tested against the top-level commands in
`ociman_pause.rs` — this note's own test deliberately only proves
each alias itself reaches the identical function with the identical
fields, not re-testing `pause`/`unpause`'s own semantics a second
time.

All 19 tests in `tests/tests/ociman_container.rs` pass (18 prior + 1
new).

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (122
test-result blocks, 0 failures — no new test file added, so the block
count is unchanged from `0492`; needed one retry under the same
unusually heavy concurrent load flagged in `0492`, one transient
`ocicri_container.rs` capabilities-class failure independently
confirmed passing in isolation, then a clean full run with
`RUST_TEST_THREADS=2`), `python3 ci/guards.py` (clean), `cargo deny
check` (clean), `bash ci/native-ci.sh` (clean on the first attempt,
run with `RUST_TEST_THREADS=2` from the start given the same ongoing
load), `bash ci/build-deb.sh` (clean, real `dpkg -i`/`--version`/
`dpkg -r` round trip). No benchmark re-run needed: neither `ociman
container pause` nor `unpause` is exercised by `ci/bench.sh`, and this
is a pure dispatch-reuse addition touching no existing function's
body at all.

## Deliberately still out of scope

- The rest of real podman's own `podman container <verb>` family not
  yet ported as aliases: `top`, `logs`, `cp`, `diff`, `commit`,
  `rename`, `restart`, `wait`, `run`, `create`, `exec`, `attach`,
  `export`, `port`, `mount`/`unmount`, `init`, `stats`, `runlabel` —
  each a pure-alias candidate of the same shape as this one and
  `0488`-`0492`, left for future increments to keep each one
  individually small and independently verified.
</content>
