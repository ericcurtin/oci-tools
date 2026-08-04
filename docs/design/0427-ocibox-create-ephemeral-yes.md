# Design note 0427: `ocibox create --yes` / `ocibox ephemeral --yes`

Status: implemented
Scope: `bin/ocibox/src/main.rs`, `tests/tests/ocibox_create.rs`,
`tests/tests/ocibox_ephemeral.rs`, `README.md`.

## What this closes

`ocibox create` had no `--yes`/`-Y` flag at all, even though its own
existing `--pull` doc comment already referenced "implying `--yes`
on the real thing" — the flag itself had never actually been added
to the CLI surface. This closes that gap for both `ocibox create`
and `ocibox ephemeral` (which real distrobox inherits the identical
flag onto).

## Real, checked-directly confirmation

`~/git/distrobox/internal/cli/create.go:76-80`: `&cli.BoolFlag{Name:
"yes", Aliases: []string{"Y"}, ...}`. `~/git/distrobox/pkg/commands/
create.go`'s own `askPullImage`:

```go
if opts.ContainerAlwaysPull || !c.containerManager.ImageExists(ctx, containerImage) {
    skipConfirm := opts.NonInteractive || opts.ContainerAlwaysPull || opts.DryRun
    if !skipConfirm {
        msg := fmt.Sprintf("Image '%s' not found.\n. Do you want to pull the image now?", ...)
        answer := c.prompter.Prompt(msg, true)
        ...
    }
    err := c.containerManager.PullImage(...)
```

Confirmed directly: `--yes` only ever skips this one real interactive
confirmation prompt before an implicit pull. This project's `ocibox`
has no interactive terminal session concept whatsoever — every
invocation already pulls silently, unconditionally, with no prompt
to skip in the first place — the same "nothing to skip" reasoning
`ocibox rm --force`'s own doc comment already gives for its own
flag.

`~/git/distrobox/internal/cli/ephemeral.go:19-32`: `newEphemeral
Command` builds its own flag list by copying every flag from
`newCreateCommand` except `compatibility`/`no-entry` — `--yes`/`-Y`
included. `~/git/distrobox/pkg/commands/ephemeral.go:72`:
`createOpts.NonInteractive = true` (hardcoded) — real `distrobox
ephemeral` accepts the flag but never actually needs it, since it
always forces the non-interactive path internally regardless. This
project's own `ocibox ephemeral` already mirrors every other flag
`create` has for the identical documented reason (`--pull`/
`--hostname`/`--home`/`--volume`/`--platform`); `--yes` now
completes that mirroring.

## Implementation

- `Command::Create` gains `yes: bool` (`#[arg(long = "yes", short =
  'Y')]`), accepted and ignored (matching the exact `rm --force`/
  `--rm-home` "accepted for compatibility, changes nothing"
  convention already established in this same file).
- `Command::Ephemeral` gains the identical flag for the identical
  reason.
- Both dispatch match arms bind it as `yes: _` — never read, the
  same pattern `Rm`'s own `force: _`/`rm_home: _` already use.

## Tests

Two new tests: `create_yes_flag_is_accepted_and_behaves_identically`
(`tests/tests/ocibox_create.rs`, also confirming `-Y` behaves
identically to `--yes`) and `ephemeral_yes_flag_is_accepted_and_
behaves_identically` (`tests/tests/ocibox_ephemeral.rs`). All 6 prior
tests in `ocibox_create.rs` and all 5 prior tests in `ocibox_
ephemeral.rs` continue to pass unmodified (8/8, 7/7 total).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean, no diff), `cargo clippy --workspace --all-targets --locked --
-D warnings`, `cargo test --workspace --locked` (119 test-result
blocks, 0 failures, clean on the first full run), `python3 ci/
guards.py`, `cargo deny check`, `bash ci/native-ci.sh` (clean,
119/119), `bash ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg
-r` round trip). Touches only CLI-surface compatibility flags, not
any hot path at all — no benchmark re-run needed.
