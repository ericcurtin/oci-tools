# Design note 0522: `ocibox enter --yes`/`-y`

Status: implemented
Scope: `bin/ocibox/src/main.rs`, `tests/tests/ocibox_enter.rs`.

## What this closes

Real `distrobox enter --yes` had no `ocibox enter` equivalent at all
-- a real CLI flag `ocibox` would reject as unrecognized if a script
or muscle-memory habit passed it.

## Real, checked-directly confirmation -- and a real, separate, pre-existing gap found along the way

- `~/git/distrobox/internal/cli/enter.go:52-56`: `--yes`/`-y`
  registered on `enter`.
- `~/git/distrobox/internal/cli/enter.go:107-115` +
  `offerCreateMissing` (`enter.go:141-176`): real `distrobox enter
  somename` on a box that **doesn't exist yet** doesn't just error --
  it offers to auto-create one, from `cfg.DefaultContainerImage`, via
  a real interactive confirmation prompt (`"Create it now, out of
  image %s?"`, defaulting to "yes" even on a bare Enter). `--yes`'s
  only real effect is skipping that specific prompt, unconditionally
  proceeding straight to auto-create either way.

This project's own `enter_and_get_exit_code` (confirmed directly: no
auto-create logic anywhere in it) has never had any equivalent of
that flow at all -- a missing box is always the exact same immediate
`"{name}: no such box"` error, matching real distrobox's own
*declined*-the-prompt outcome unconditionally, never its own true
default (auto-create) one. This is a real, separate, pre-existing
divergence, honestly documented in `Command::Enter`'s own doc comment
now (it was never previously flagged in any design note, including
`0207`, `enter`'s own origin) rather than papered over while wiring
this flag -- and it's a genuinely bigger gap than a single flag, so
it stays deliberately unclosed here. Because of it, `--yes` has
nothing to skip regardless of whether it's given: a missing box
errors either way, matching the identical "nothing to skip" no-op
class `0514`/`0517`/`0518` already established, just for a real
reason specific to this command rather than the simpler "no
interactive prompt anywhere at all" reasoning those three share.

## Implementation

`Command::Enter` gains `yes: bool` (`#[arg(long = "yes", short =
'y')]` -- lowercase `y`, matching real distrobox's own alias for
`enter` specifically, unlike `rm`/`create`/`stop`'s own uppercase
`-Y`), accepted and immediately discarded (`yes: _`) at the one call
site. `cmd_enter`'s own signature is untouched. `Command::Ephemeral`
already has its own separate `yes` field (inherited from `create`'s
own flag set, matching real `distrobox ephemeral`'s own composition)
-- unaffected by this change.

## Tests

One new integration test in `tests/tests/ocibox_enter.rs`:
`enter_yes_flag_is_accepted_and_behaves_identically` -- proven both
ways: still a clear error on an unknown box with `--yes` given, and
still a real, successful enter on a real one with `--yes` given.

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (123
test-result blocks -- no new test file added, so the block count is
unchanged from `0521`; clean on the first attempt with
`RUST_TEST_THREADS=2`), `python3 ci/guards.py` (clean), `cargo deny
check` (clean), `bash ci/native-ci.sh` (clean on the first attempt
with `RUST_TEST_THREADS=2`), `bash ci/build-deb.sh` (clean on the
first attempt, real `dpkg -i`/`--version`/`dpkg -r` round trip). Pure
CLI-parsing addition -- no hot path touched, no `ci/bench.sh` rerun
needed.

## Deliberately still out of scope

The real "offer to auto-create a missing box" flow itself (see
above) remains a genuinely separate, larger gap -- closing it would
need a real interactive-vs-non-interactive branch this project's own
architecture has never had anywhere (every other command here is
already unconditionally non-interactive), plus a decision about
what `cfg.DefaultContainerImage`'s own equivalent would even be for
this project (real distrobox defaults to a configurable base image
this project has no matching config concept for at all). `ociman
image mount`/`unmount`'s own bare-mode listing/`--format` and
`ocibox upgrade`/`export --app` remain the other open candidates from
prior notes.
</content>
