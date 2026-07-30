# Design note 0348: `ociman volume ls -q`/`--quiet`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_volume.rs`.

## What this closes

`ociman volume ls` had `--format` but no `-q`/`--quiet` at all — a
real, narrow gap flagged after `0347`, and the last one from that
survey.

## Real, checked-directly semantics

Read `~/git/podman/cmd/podman/volumes/list.go` directly:

- `--quiet`/`-q` renders exactly `{{.Name}}\n` per volume — names
  only, one per line, no header at all.
- `--quiet` and `--format` together is a real, immediate error
  (`errors.New("quiet and format flags cannot be used together")`),
  checked *before* anything else in the command's own `RunE`.
- An empty store under `--quiet` prints nothing at all — this
  project's own friendly `"no volumes"` empty-state message is
  specific to the default table (`0263`'s own established convention
  for `ociman images`/`ps`, kept for internal consistency there), not
  something `--quiet` ever shows for *any* list command in this
  project (matching real podman's/docker's own identical behavior:
  quiet mode is meant to be script-parseable, an extra sentence would
  defeat that).

## Implementation

`VolumeCommand::Ls` gained `quiet: bool` (`-q`/`--quiet`, the same
short/long pair `ps`/`images` already use for the identical concept).
`cmd_volume_ls` checks the `--format`-conflict first (matching real
podman's own check ordering), then branches for quiet *before* the
existing `--json`/table logic — a plain `for record in &records {
println!("{}", record.name); }`, no new primitive needed at all.

## Verified

New tests in `ociman_volume.rs`:
`volume_ls_quiet_prints_only_names_with_no_header` (both `-q` and
`--quiet` checked to behave identically),
`volume_ls_quiet_on_an_empty_store_prints_nothing`,
`volume_ls_quiet_and_format_together_is_a_clear_error`.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (113 test-result blocks,
0 failures), `python3 ci/guards.py`, `cargo deny check`.

## Still ahead

Real `podman volume ls --filter` (label/dangling-aware filtering) —
this project's own volumes currently have no label metadata to filter
on at all, a genuinely bigger feature (would need `VolumeRecord`'s own
on-disk shape extended first) than anything surveyed in this recent
`volume` pass, deliberately deferred rather than half-implemented.
