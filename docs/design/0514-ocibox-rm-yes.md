# Design note 0514: `ocibox rm --yes`/`-Y`

Status: implemented
Scope: `bin/ocibox/src/main.rs`, `tests/tests/ocibox_list_rm.rs`.

## What this closes

Real `distrobox rm --yes`/`-Y` had no `ocibox rm` equivalent at all --
a real CLI flag `ocibox` would reject as unrecognized (confirmed
live: `ocibox rm --yes somebox` -> `error: unexpected argument
'--yes' found`), unlike `--force`/`--rm-home` (already accepted,
`0321`/`0405`) if a script or muscle-memory habit passed it. `--yes`
already exists on `ocibox create`/`ocibox ephemeral` (`0427`) but was
simply missed on `rm` -- a genuinely fresh gap, not a re-examined old
deferral.

## Real, checked-directly confirmation

- `~/git/distrobox/internal/cli/rm.go:31-36`: `--yes`/`-Y` registered
  alongside `--all`/`--force`/`--rm-home`.
- `~/git/distrobox/pkg/commands/rm.go:82`: `if !options.Force &&
  !options.NoTTY && len(distroboxesToRemove) > 0 { ...prompt... }` --
  the top-level "do you really want to delete containers" prompt,
  skipped by *either* `--force` or `--yes`/`-Y` (`NoTTY`).
- `~/git/distrobox/pkg/commands/rm.go:135`: `if !forceRemove &&
  !noTTY && container.IsRunning() { ...prompt... }` -- the
  per-container "container is running, force delete it?" prompt,
  same either-flag gate.
- `~/git/distrobox/pkg/commands/rm.go:150`: `if removeHomeRequested
  && !noTTY && ... { ...prompt... }` -- `--rm-home`'s own prompt,
  gated on `!noTTY` alone (not `--force`), already established as a
  real no-op by `0405` since this project has no interactive
  terminal session concept whatsoever.

Every one of these prompts is something this project's own `ocibox`
never shows in the first place (every invocation is already the
real, checked-directly equivalent of real distrobox's own
always-`--yes`/`noTTY` case) -- so `--yes`/`-Y` has nothing left to
skip here either, the identical "accepted for real CLI compatibility
but changes nothing" reasoning `--force`/`--rm-home` already use.

## Implementation

`Command::Rm` gains `yes: bool` (`#[arg(long = "yes", short =
'Y')]`), accepted and immediately discarded at the one call site
(`yes: _`, matching `force`/`rm_home`'s own existing pattern).
`cmd_rm`'s own signature is untouched.

## Tests

One new integration test in `tests/tests/ocibox_list_rm.rs`:
`rm_yes_flag_is_accepted_and_behaves_identically` -- a real box, still
genuinely removed exactly as a plain `rm` would with `--yes` given
alongside it.

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (122
test-result blocks, 0 failures -- no new test file added, so the
block count is unchanged from `0513`; clean on the first attempt
with `RUST_TEST_THREADS=2`), `python3 ci/guards.py` (clean), `cargo
deny check` (clean), `bash ci/native-ci.sh` (the documented transient
`ocicri_container.rs` flakiness under this host's own persistent CPU
contention showed up once, in a test entirely unrelated to this
change, confirmed transient by rerunning it in isolation -- passed --
then a fully clean run with `RUST_TEST_THREADS=1` throughout), `bash
ci/build-deb.sh` (clean on the first attempt, real `dpkg -i`/
`--version`/`dpkg -r` round trip). Pure CLI-parsing addition -- no
hot path touched, no `ci/bench.sh` rerun needed.

## Deliberately still out of scope

`ocibox list --no-color` (another likely faithful no-op, since this
project's own list output has no ANSI/color codes anywhere) and
`ocibox create` auto-generating a desktop entry by default (a real
default-behavior change needing existing `create` tests' `$HOME`
environment audited first) remain open candidates for a future
increment.
</content>
