# Design note 0517: `ocibox create --no-entry`

Status: implemented
Scope: `bin/ocibox/src/main.rs`, `tests/tests/ocibox_create.rs`.

## What this closes

Real `distrobox create --no-entry` had no `ocibox create` equivalent
at all -- a real CLI flag `ocibox` would reject as unrecognized if a
script or muscle-memory habit passed it. Independently found this
turn while re-examining `0515`'s own "still out of scope" note about
`ocibox create` auto-generating a desktop entry by default.

## Real, checked-directly confirmation

- `~/git/distrobox/internal/cli/create.go:156-161`: `--no-entry` bool
  flag registered on `create`.
- `~/git/distrobox/internal/cli/create.go:200`: `generateEntry :=
  cfg.GenerateEntry && !cmd.Bool("no-entry")`.
- `~/git/distrobox/pkg/commands/create.go:163`: `if
  opts.GenerateEntry && !opts.DryRun && !opts.Rootful {
  ...generateEntryCmd.Execute... }` -- the only real effect of
  `--no-entry`: suppressing an automatic desktop-entry generation
  `create` would otherwise perform right after a successful create.
- `~/git/distrobox/internal/cli/ephemeral.go:22-24`: real distrobox's
  own `ephemeral` command explicitly strips `--no-entry` back out of
  its own inherited flag set (`ignoredFlags`), rather than accepting-
  and-ignoring it -- `ephemeral.go:101`/`pkg/commands/ephemeral.go:70`
  hardcode `GenerateEntry: false` unconditionally, with no flag
  surface to override it at all.

`ocibox create` never performs that automatic entry-generation step
in the first place (desktop-entry generation here is still its own
separate, always-manually-invoked `ocibox generate-entry`, `0364`;
adding it as a default behavior is the real, deliberately still-
deferred gap `0515` already documented, needing existing `create`
tests' `$HOME` environment audited first, not just a flag). Since the
behavior `--no-entry` would suppress already never happens either
way, the flag is a genuine, faithful no-op today.

## Implementation

`Command::Create` gains `no_entry: bool` (`--no-entry`), accepted and
immediately discarded (`no_entry: _`) at the one call site.
`cmd_create`'s own signature is untouched. `Command::Ephemeral`
deliberately gains no such field at all, matching real `distrobox
ephemeral`'s own explicit absence rather than a flag that would be
accepted-and-ignored.

## Tests

One new integration test in `tests/tests/ocibox_create.rs`:
`create_no_entry_flag_is_accepted_and_behaves_identically` -- run
against a real, dedicated temporary `$HOME` (never the real ambient
one running the test suite, for safety regardless of the current
no-op status), proving the box is still genuinely created and no
`.desktop` entry appears either way.

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (122
test-result blocks, 0 failures -- no new test file added, so the
block count is unchanged from `0516`; clean on the first attempt
with `RUST_TEST_THREADS=2`), `python3 ci/guards.py` (clean), `cargo
deny check` (clean), `bash ci/native-ci.sh` (clean on the first
attempt with `RUST_TEST_THREADS=2`), `bash ci/build-deb.sh` (clean on
the first attempt, real `dpkg -i`/`--version`/`dpkg -r` round trip).
Pure CLI-parsing addition -- no hot path touched, no `ci/bench.sh`
rerun needed.

## Deliberately still out of scope

`ocibox create` actually generating a desktop entry by default
(real distrobox's own true default behavior, `container_generate_
entry: "true"`) remains the separate, larger, deliberately-deferred
gap `0515` already documented -- this note only closes the pure
CLI-compatibility half. `ocibox stop` (a new no-op subcommand, real
distrobox's own `stop` has no equivalent target here at all since a
box has no persisted running state, `0207`/`0515`) and `ociman image
mount`/`unmount` (real, separate, non-alias subcommands calling a
genuinely different image-specific engine method than the container
`mount`/`unmount` this project already has, correcting a
mischaracterization repeated across `0481`/`0482`/`0499`) remain open
candidates for future increments.
</content>
