# Design note 0494: `ociman container restart` alias

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_container.rs`.

## What this closes

Continuing the `ociman container` alias family `0357`/`0431`/`0474`/
`0488`-`0493` started: `restart` — the eighth member of real podman's
own `podman container <verb>` family closed so far — was still
missing.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/restart.go:23-93`:
  `containerRestartCommand` (`Parent: containerCmd`) and top-level
  `restartCommand` share the exact same `Use`/`Short`/`Long`/`RunE`/
  `Args`/`ValidArgsFunction`, and both get the identical flag set
  applied via the one shared `restartFlags(cmd)` helper (`--all`/
  `-a`, `--running`, `--cidfile`, `--filter`/`-f`, `--time`/`-t`)
  followed by the identical `validate.AddLatestFlag` call — a
  byte-identical alias, the same shape `0492` already established
  for `Self::Kill`.

## Implementation

`ContainerCommand::Restart` is a new variant, field-for-field
identical to the already-existing `Command::Restart` (`ids`, `time`,
`all`, `cidfile`, `filter`, `latest`), dispatching into the exact same
`cmd_restart` function `ociman restart` itself already calls with the
identical argument order — zero new business logic, zero new
primitive, the same "raw fields straight through" shape `0489`/
`0490`/`0492`/`0493` already used.

(Real podman's own `--running` flag is not ported here either: this
project's own top-level `Command::Restart` has never implemented it —
the same honestly narrower first-slice scope `0491`'s `Start` variant
already mirrored for `--all`/`--filter`/`--interactive`, applied here
identically rather than inventing a wider alias than the aliased
command itself supports.)

## Tests

One new integration test added to `tests/tests/ociman_container.rs`:

- `container_restart_is_a_byte_identical_alias_for_top_level_restart`
  — proves the alias actually restarts a real, running container
  (stops it, then starts it again, ending back at `running`), exactly
  like the top-level command.

While fixing an unrelated typo during this same edit, a copy-paste
mistake in an earlier draft of this change briefly and accidentally
replaced `&running_id` with `&id` in the pre-existing, unrelated
`container_prune_removes_created_and_stopped_but_not_running` test's
own final cleanup line (an `Edit` tool ambiguous-match mistake, caught
immediately by the resulting compile error — `id` isn't in scope
there at all — before ever running or committing it); reverted before
proceeding, confirmed via `git diff` that only the intended two hunks
(the header doc comment and the new test appended at the true end of
the file) remain.

Full `restart` semantics (multi-id resolve-then-act, `--all`,
`--cidfile`, `--filter`, `--latest`, `--time`, the real
"never-started-or-stopped-is-simply-started, paused-is-a-real-error"
state rules) are already exhaustively tested against the top-level
command in `ociman_start.rs`/`ociman_stop.rs` — this note's own test
deliberately only proves the alias itself reaches the identical
function with the identical fields, not re-testing `restart`'s own
semantics a second time.

All 20 tests in `tests/tests/ociman_container.rs` pass (19 prior + 1
new).

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (122
test-result blocks, 0 failures — no new test file added, so the block
count is unchanged from `0493`; clean on the first attempt with
`RUST_TEST_THREADS=2`, run preemptively given the same unusually
heavy concurrent load flagged in `0492`/`0493`), `python3 ci/
guards.py` (clean), `cargo deny check` (clean), `bash ci/
native-ci.sh` (clean on the first attempt, also run with
`RUST_TEST_THREADS=2`), `bash ci/build-deb.sh` (clean, real `dpkg -i`/
`--version`/`dpkg -r` round trip). No benchmark re-run needed:
`ociman container restart` is not exercised by `ci/bench.sh`, and this
is a pure dispatch-reuse addition touching no existing function's
body at all.

## Deliberately still out of scope

- The rest of real podman's own `podman container <verb>` family not
  yet ported as aliases: `top`, `logs`, `cp`, `diff`, `commit`,
  `rename`, `wait`, `run`, `create`, `exec`, `attach`, `export`,
  `port`, `mount`/`unmount`, `init`, `stats`, `runlabel` — each a
  pure-alias candidate of the same shape as this one and `0488`-
  `0493`, left for future increments to keep each one individually
  small and independently verified.
- Real podman's own richer `podman restart`/`podman container
  restart --running` (restart only currently-running containers,
  skipping the rest) — a genuinely separate, still-open gap in the
  *top-level* `ociman restart` itself (not something this alias
  increment introduces or could close on its own), left for its own
  future increment.
</content>
