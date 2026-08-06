# Design note 0515: `ocibox list --no-color`

Status: implemented
Scope: `bin/ocibox/src/main.rs`, `tests/tests/ocibox_list_rm.rs`.

## What this closes

Real `distrobox list --no-color` had no `ocibox list`/`ocibox ls`
equivalent at all -- a real CLI flag `ocibox` would reject as
unrecognized if a script or muscle-memory habit passed it.

## Real, checked-directly confirmation

- `~/git/distrobox/internal/cli/list.go:22-26`: `--no-color` bool
  flag registered on `list`.
- `~/git/distrobox/internal/cli/list.go:44-46`: `noColor :=
  cmd.Bool("no-color") || !isTerminal()` -- real distrobox's own
  `list` is *already* colorless whenever stdout isn't a real tty
  (every automated/piped invocation, including every test harness
  and every non-interactive call this project's own equivalent would
  ever make).
- `~/git/distrobox/internal/cli/list.go:50-67` (`printResult`): the
  only real effect of `noColor` being `true` -- skipping `ui.Green`/
  `ui.Yellow` ANSI highlighting applied per row based on each
  container's own running state.

This project's own `ocibox list` has no ANSI color codes anywhere at
all (confirmed by grep) -- a direct, honest consequence of a real,
separate, pre-existing gap this note also corrects a stale doc
comment about: `Command::List`'s own doc comment previously claimed
real distrobox's own container-status column was merely "not yet"
added, pending a still-future `ocibox enter`. `ocibox enter` has
long since landed, and status still doesn't apply: unlike `ociman`'s
own containers, a box has no distinct running/stopped state to
report at all -- `ocibox enter` runs a fresh, live command each time
rather than starting/stopping one persisted process. Since there is
no running-vs-stopped distinction to color by in the first place,
`--no-color` has nothing to disable here either.

## Implementation

`Command::List` (previously a bare unit variant) gains `no_color:
bool` (`--no-color`), accepted and immediately discarded (`no_color:
_`) at the one call site. `cmd_list`'s own signature is untouched.

## Tests

One new integration test in `tests/tests/ocibox_list_rm.rs`:
`list_no_color_flag_is_accepted_and_behaves_identically` -- a real
box's own `list` output is byte-for-byte identical with and without
`--no-color`, and contains no ANSI escape codes (`\x1b`) either way.

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (122
test-result blocks, 0 failures -- no new test file added, so the
block count is unchanged from `0514`; clean on the first attempt
with `RUST_TEST_THREADS=2`), `python3 ci/guards.py` (clean), `cargo
deny check` (clean), `bash ci/native-ci.sh` (clean on the first
attempt with `RUST_TEST_THREADS=2`), `bash ci/build-deb.sh` (clean on
the first attempt, real `dpkg -i`/`--version`/`dpkg -r` round trip).
Pure CLI-parsing addition -- no hot path touched, no `ci/bench.sh`
rerun needed.

## Deliberately still out of scope

Real color output itself (green/yellow highlighting by running
state) remains unimplemented -- correctly so, since it depends on
the same box-status tracking this project's own architecture has no
equivalent of at all (see the corrected doc comment above). Adding
real per-box running-state tracking would be a genuinely larger,
separate feature, not a small CLI-compatibility increment. `ocibox
create` auto-generating a desktop entry by default (a real default-
behavior change needing existing `create` tests' `$HOME` environment
audited first) and `ociman container update` (a correct but larger,
21-field alias) remain open candidates for a future increment.
</content>
