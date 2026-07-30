# Design note 0364: `ocibox generate-entry`

Status: implemented
Scope: `bin/ocibox/src/main.rs`, `tests/tests/ocibox_generate_entry.rs`,
`README.md`.

## What this closes

Named as a remaining gap in `ocibox`'s own subcommand surface since
`0322` (repeated in every export-related note through `0333`): real
`distrobox generate-entry` had no equivalent here. Unlike `stop`
(architecturally blocked on a persistent-container model this project
doesn't have yet) and `assemble` (a whole declarative manifest
feature), this one turned out to be a genuine composition play once
`export --app`'s own icon/label/desktop-file-writing machinery
(`0322`/`0327`/`0328`) had matured — the "materially bigger" framing
those earlier notes gave it, as a blanket group with `upgrade`/
`assemble`, no longer holds for this one specifically.

## Real, checked-directly semantics

Read `~/git/distrobox/pkg/commands/generate_entry.go` and
`~/git/distrobox/internal/cli/generate-entry.go` directly:
`container-name` is a single positional arg (`cmd.Args().First()`),
`--all`/`-a`, `--delete`/`-d`, `--icon`/`-i` (default `"auto"`). The
generated file lives at `<desktopEntryBaseDir>/applications/
<name>.desktop` (`getEntryFilePath`) — the exact same
`$HOME/.local/share/applications` directory `export --app` already
defaults to. The template (`assets/desktop_entry.toml.tmpl`) writes an
`Exec=distrobox enter <name>` launcher plus a `[Desktop Action
Remove]` running `distrobox rm <name>` — ported here as `ocibox enter
<name>`/`ocibox rm <name>` (`ocibox rm` needs no `--force` at all,
matching this project's own already-established no-op-force
convention, so the generated action needs none either).

Two real, deliberate divergences, found and documented rather than
silently drifted into:

- **Icon default.** Real distrobox's own `"auto"` default detects a
  distro from the box's own image name and downloads its logo over
  the network the first time, caching it locally after
  (`resolveIcon`/`downloadIconFile`) — a real network dependency this
  narrower first slice deliberately doesn't reproduce. Falls back
  instead to a fixed, standard freedesktop icon name every icon theme
  already provides (`utilities-terminal`) — not real distrobox's own
  separately-*installed*, non-standard fallback asset
  (`terminal-distrobox-icon`, a file this project never installs, so
  referencing its bare name here would only ever resolve to a missing
  icon on a host without real distrobox itself present). `--icon`,
  when given explicitly, always overrides this, matching real
  distrobox's own identical pass-through for any non-`auto` value.
- **No implicit default name.** Real distrobox's own `resolveTargets`
  falls back to a hardcoded `"my-distrobox"` when neither a name nor
  `--all` is given at all — a real, checked-directly quirk with no
  existence check anywhere in that one code path, so it would
  generate an entry for a box that might not even exist. `ocibox
  create` has never had an implicit default name of its own; this
  command doesn't invent one either, giving a clear, immediate error
  instead.

A third, smaller finding: real distrobox's own `deleteEntry` for this
command has no marker/ownership check at all (a bare
`os.Remove(entryFilePath)`, tolerating `os.IsNotExist`) — a real,
checked-directly *asymmetry* with `export --app --delete`'s own more
cautious marker check. This project matches each command's own real,
independently-checked behavior rather than assuming one uniform safety
convention applies everywhere: `generate-entry --delete` here likewise
performs no marker check, even though the file this command itself
writes does still carry a small, purely informational identifying
comment (`# ocibox_generate_entry`) real distrobox's own template
never had at all.

## Implementation

New `Command::GenerateEntry { name: Option<String>, all: bool, delete:
bool, icon: Option<String> }`; `cmd_generate_entry` resolves targets
(every existing box via `list_boxes()` for `--all`, otherwise the one
given name, existence-checked unless `--delete`), then either removes
or writes `<name>.desktop` under [`default_app_export_path`] (reused
verbatim from `export --app`) via a plain `format!`-built string — no
external template engine, matching this project's own established
convention (`rewrite_desktop_file`'s own line-by-line approach) rather
than introducing one for a single new command.

## Verified

New `tests/tests/ocibox_generate_entry.rs`, mirroring
`ocibox_export.rs`'s own established fully-offline `seed_image`+
`ocibox_with_home` pattern:
`generate_entry_writes_a_real_desktop_launcher_with_the_default_icon`;
`generate_entry_icon_overrides_the_default`;
`generate_entry_delete_removes_the_launcher_and_tolerates_a_missing_one`;
`generate_entry_all_covers_every_existing_box` (also confirms a `NAME`
given alongside `--all` is genuinely ignored);
`generate_entry_of_an_unknown_box_is_a_clear_error_but_delete_tolerates_it`;
`generate_entry_requires_either_a_name_or_all`.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures, full clean
run, no flakes), `python3 ci/guards.py`, `cargo deny check`, `bash
ci/native-ci.sh`, `bash ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).
