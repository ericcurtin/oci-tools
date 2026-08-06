# Design note 0518: `ocibox stop`

Status: implemented
Scope: `bin/ocibox/src/main.rs`, `tests/tests/ocibox_list_rm.rs`.

## What this closes

`ocibox`'s own module-level doc comment has flagged `stop` as "still
ahead" since `0207` (over 300 increments ago) and never revisited it.
A real CLI command `ocibox` would reject as unrecognized entirely.

## Real, checked-directly confirmation

- `~/git/distrobox/internal/cli/stop.go:17-42`: `stop` registers
  `--all`/`-a` and `--yes`/`-Y`, falls back to a configured "default
  container name" with neither given (a whole separate concept this
  project doesn't have at all, the identical restriction `ocibox
  rm`'s own doc comment already establishes for `rm`).
- `~/git/distrobox/pkg/commands/stop.go:37-69` (`Execute`): resolves
  names (`--all` sweeps every listed container; `ErrEmptyContainerList`
  if none exist at all), prompts unless `--yes`/`--force`-equivalent
  `NonInteractive`, then calls `containerManager.Stop`. No success-
  path `Println` anywhere in this function.
- `~/git/distrobox/pkg/containermanager/providers/podman.go:634-643`
  (`Stop`): shells out to a real `podman stop <names>` -- the *only*
  real effect `stop` ever has.
- `~/git/distrobox/internal/cli/stop.go:80-87` (`stopAction`):
  `ErrEmptyContainerList` is caught and printed as a non-fatal
  `"No containers found."` to stderr, still exiting `0`.

This project's own boxes have no persisted running state at all
(`0207`/`0515` -- `ocibox enter` runs a fresh, live command each time
rather than starting/stopping one long-lived process), so real
`podman stop <name>`'s own real target simply doesn't exist here --
`stop` is a genuine, faithful no-op, the same class of finding
`0512`/`0513` already established.

A real, deliberate divergence from `ocibox rm` worth calling out
explicitly: real `distrobox rm` has its own distinct
`warnUnknownContainers` function specifically carving out tolerance
for a name that doesn't resolve to anything (ported here as `0321`'s
own "warning, not a hard error" behavior) -- real `distrobox stop`
has no equivalent of that at all. An unknown name there is a genuine,
hard failure: the real, propagated `podman stop somename` error
`containerManager.Stop` never catches or downgrades. `ocibox stop`
matches that same hard-failure shape rather than `rm`'s own tolerant
one.

## Implementation

New `Command::Stop { names: Vec<String>, all: bool, yes: bool }`.
`cmd_stop`:
- `--all`: lists every box (`list_boxes`); prints `"No containers
  found."` to stderr (not an error) if there are none at all,
  matching real distrobox's own checked-directly non-fatal message.
  Otherwise, does nothing else at all.
- Explicit names: requires at least one (matching `rm`'s own "at
  least one name, or `--all`, is required" restriction); validates
  each (`validate_box_name`, the same defensive charset check `rm`
  already applies) and confirms it resolves to a real, already-
  existing box directory, erroring immediately (`"{name}: no such
  box"`, this project's own already-established wording) on the
  first one that doesn't -- matching real distrobox's own genuine
  hard failure there, not `rm`'s own tolerant one.
- Otherwise: prints nothing at all on success, matching real
  distrobox's own identical silence.

`--yes`/`-Y` is accepted and immediately discarded (`yes: _`), the
same "accepted for real CLI compatibility but changes nothing"
convention `ocibox rm --yes` (`0514`) already established.

## Tests

Six new integration tests in `tests/tests/ocibox_list_rm.rs`:
- `stop_on_a_real_box_is_a_real_no_op_and_never_removes_it`
- `stop_yes_flag_is_accepted_and_behaves_identically`
- `stop_of_an_unknown_name_is_a_clear_error`
- `stop_requires_a_name_or_all`
- `stop_all_on_an_empty_store_succeeds_with_a_message`
- `stop_all_on_existing_boxes_is_a_real_no_op`

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (122
test-result blocks, 0 failures -- no new test file added, so the
block count is unchanged from `0517`; clean on the first attempt
with `RUST_TEST_THREADS=2`), `python3 ci/guards.py` (clean), `cargo
deny check` (clean), `bash ci/native-ci.sh` (clean on the first
attempt with `RUST_TEST_THREADS=2`), `bash ci/build-deb.sh` (clean on
the first attempt, real `dpkg -i`/`--version`/`dpkg -r` round trip).
A new, always-inert-once-validated command -- no hot path touched,
no `ci/bench.sh` rerun needed.

## Deliberately still out of scope

`ociman image mount`/`unmount` (real, separate, non-alias
subcommands calling a genuinely different image-specific engine
method than the container `mount`/`unmount` this project already
has, correcting a mischaracterization repeated across `0481`/`0482`/
`0499`) and `ocibox upgrade`/`export --app` (still flagged as ahead
in this project's own module-level doc comment, genuinely bigger:
`upgrade` needs a real package-manager-detection-and-invocation
story, `export --app` needs the same desktop-entry machinery
`generate-entry` already has plus per-application enumeration inside
the box) remain open candidates for future increments.
</content>
