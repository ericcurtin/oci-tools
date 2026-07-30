# Design note 0322: `ocibox export --app`

Status: implemented
Scope: `bin/ocibox/src/main.rs`, `tests/tests/ocibox_export.rs`.

## Closing `0321`'s own "still ahead"

`0321` named `export --app` (desktop-entry export) as the most self-
contained of `ocibox`'s remaining real gaps against real `distrobox`
— unlike `stop`/`upgrade`/persistence, it needs no new architectural
decisions, only new file-generation logic reusing `export`'s own
already-established `--bin` machinery (0252). This note closes it,
deliberately without its own icon-copying half — a genuinely separate,
larger increment.

## Real, checked-directly semantics

Read `~/git/distrobox/internal/inside-distrobox/assets/distrobox-export`
directly (`export_application`) rather than assumed:

- `--app`'s own value is either an absolute path to a `.desktop` file
  directly (`[ -e "${exported_app}" ]`), or a name searched for across
  canonical desktop-file directories, matching against a file's own
  `Exec=`/`Name=` line — real distrobox additionally checks
  `$XDG_DATA_DIRS`/a Flatpak-exports directory/the user's own
  `$HOME/.local/share/applications`, none of which are reachable here
  the same way (this project's own `ocibox` runs from the *host*, not
  with the box's own env — the same real, already-documented `export
  --bin` divergence from 0252) — so this slice only ever searches
  `/usr/share/applications`/`/usr/local/share/applications` inside the
  box's own rootfs, a real, honestly narrower (not silently different)
  scope.
- Real distrobox skips any `.desktop` file whose own `Exec=` already
  routes through `distrobox ... enter` (`grep -L`) — avoiding a
  double-export of an already-exported app; matched here by checking
  for `ocibox enter` in the file's own `Exec=` line.
- The rewrite itself: `Exec=` gets `distrobox enter -n <container>
  --` prepended verbatim (`sed "s|^Exec=\(.*\)|Exec=${container_
  command_prefix}\1|g"`); any `TryExec=` line is dropped entirely
  (`sed "/^TryExec=.*/d"` — it would check for the *host's* own
  binary, not the box's, so it's actively wrong to keep). The
  generated file is named `${container_name}-$(basename
  ${desktop_file})`, so exports from two different boxes of an app
  with the same launcher filename never collide.
- Icon handling (finding an app's own icon files across several
  canonical directories, copying them into the host's own
  `~/.local/share/icons`, rewriting `Icon=` to point at the copy) is
  real, separate, and materially more complex — this project has no
  icon-file/`XDG_DATA_DIRS`-search machinery of any kind yet. Left
  entirely untouched for this note: `Icon=` is copied into the
  generated file exactly as the box's own original had it, which may
  not resolve to a real, existing file on the host at all — an
  honestly-documented, narrower-than-real-distrobox gap, not a silent
  behavior change.

## Implementation

`Command::Export` gained a new `app: Option<String>` field alongside
the existing `bin: Option<String>` (previously a required `String`) —
exactly one of the two is now required, matching real distrobox's own
identical "choose only one action" rule (checked directly, its own
`if [ -n "${exported_app}" ] && [ -n "${exported_bin}" ]` guard).
`cmd_export` is now a small dispatcher; the pre-existing `--bin` logic
moved unchanged into `cmd_export_bin`, and a new `cmd_export_app`
mirrors it closely: [`find_desktop_files`] resolves `--app` to every
real, matching `.desktop` file (an explicit path used as-is, or a
name-based search across the two canonical directories, skipping
already-exported ones); [`rewrite_desktop_file`] performs the
`Exec=`-prefixing/`TryExec=`-stripping rewrite; the same
marker-comment/`--delete`-safety-check convention `EXPORT_MARKER`
already established for `--bin` gets its own `APP_EXPORT_MARKER`
sibling. `default_app_export_path` (`$HOME/.local/share/applications`)
is a real, separate default from `--bin`'s own `$HOME/.local/bin`,
matching real distrobox's own per-mode defaults exactly.

## Verified

Manual, end-to-end: a real `.desktop` file written into a box's own
rootfs, `export --app "My App"` correctly rewrites `Exec=`, strips
`TryExec=`, leaves `Icon=` untouched, and writes the result under
`<box>-<basename>.desktop`; `--delete` removes it again, refusing a
foreign file with no marker comment; an absolute in-rootfs path also
works directly as `--app`'s own value; an unknown app name is a clear
"cannot find any desktop files" error; giving both `--app` and `--bin`
(or neither) is a clear error.

Integration (`tests/tests/ocibox_export.rs`, 5 new tests, 10 total, 5
pre-existing): rewritten-desktop-file content assertions (`Exec=`
routed through `ocibox enter`, `TryExec=` gone, `Icon=`/`Name=`
untouched); explicit desktop-file-path support; delete-and-refuse-
foreign-file (mirroring the existing `--bin` test exactly); unknown-
app error; `--app`/`--bin` either/or validation.

Regression: all 10 `ocibox_export.rs` tests pass; the rest of the
`ocibox` suite (`ocibox_create.rs`, `ocibox_enter.rs`, `ocibox_
ephemeral.rs`, `ocibox_list_rm.rs`) is unaffected. Full `cargo test
--workspace --locked`: 112 test result blocks, 0 failures (two known
`ocicri_container.rs` flakes under full parallel load hit across two
`native-ci.sh` runs this turn — `remove_forcefully_kills_a_running_
container`, then `create_container_bind_mount_follows_a_symlinked_
host_path`/`create_container_bind_mount_is_genuinely_live_at_runtime`
— none touched by this change, each re-verified passing in isolation,
and a clean full re-run confirmed).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).

Performance: `ocibox export` is a one-shot, offline command, not part
of any hot-path benchmark tracked in `docs/benchmarks.md`. No
re-benchmark needed.

## Still ahead

Icon handling for `export --app` (finding/copying icon files, real
`Icon=` rewriting) remains a real, separately-scoped, deliberately
deferred future candidate — the single largest remaining gap in this
export mode specifically. Real `distrobox`'s own `stop`/`upgrade`/
`generate-entry`/`assemble` still have no `ocibox` equivalent at all
(see `0321`'s own "still ahead" for why each is a materially bigger,
separately-scoped feature).
