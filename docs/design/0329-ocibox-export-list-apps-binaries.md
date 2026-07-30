# Design note 0329: `ocibox export --list-apps`/`--list-binaries`

Status: implemented
Scope: `bin/ocibox/src/main.rs`, `tests/tests/ocibox_export.rs`,
`README.md`.

## What this closes

Real `distrobox export --list-apps`/`--list-binaries` (list every
application/binary already exported from a box) had no equivalent in
`ocibox export` at all — flagged in the `0327`/`0328` surveys as small,
remaining, self-contained `export` gaps.

## Real, checked-directly semantics

Read real `distrobox-export`'s own `list_exported_applications`/
`list_exported_binaries` directly (`~/git/distrobox/internal/inside-
distrobox/assets/distrobox-export:609-689`) rather than guessed. Both
scan a search directory for files, filter down to genuinely-exported
ones, and print `%-20s | %-30s\n` (name, path):

- **Apps**: searches `/run/host$HOME/.local/share/applications`
  (`ocibox`'s own host-side equivalent: `--export-path` or
  [`default_app_export_path`]), keeping only files whose own `Exec=`
  line routes through `distrobox ... enter`, then further filters to
  ones whose own *path* contains the real `$CONTAINER_ID` substring
  (the container's own name, per real distrobox's own naming) —
  matching this project's own already-`box`-prefixed export filenames.
  Its own displayed name strips the `--export-label` back off the
  `Name=` value via `sed 's|(.*)||g'` (drop everything from the first
  `(` onward).
- **Binaries**: searches `--export-path`/[`default_export_path`],
  keeping only files containing the `# distrobox_binary` marker
  comment, then further filters by a `# name: <container_name>`
  comment line. Its own displayed name is extracted by a fragile,
  template-specific `grep -B1 "fi" | grep exec | cut -d' ' -f2`.

## Adaptations for this project's own, different implementation

This project already has a real, more precise per-box filter than real
distrobox's own path-substring check: every export already carries a
`# box: <box_name>` marker comment (`APP_EXPORT_MARKER`/
`EXPORT_MARKER`'s own established template, `0252`/`0322`), so
`--list-apps`/`--list-binaries` reuse that directly (a new, shared
`exported_files_for_box(export_dir, box_name, marker)` helper) instead
of a path-substring heuristic — genuinely more robust (no risk of a
box named, say, `"box1"` accidentally matching a file actually
belonging to `"box10"`, which a naive substring check on the file path
could).

Real distrobox's own binary-name extraction (`grep -B1 "fi" | grep
exec | ...`) is tightly coupled to its own, much more elaborate wrapper
template (with a real `if`/`fi` conditional inside); this project's own
wrapper is a single `exec` line with no `fi` at all, so that logic
doesn't apply here regardless. Since `cmd_export_bin`'s own destination
filename already *is* exactly the exported binary's own basename (no
box-name prefix, unlike `--app`'s own desktop-file naming), the
exported file's own name is used directly as the displayed name
instead — equivalent information, without any fragile re-parsing.

The app-name label-stripping (`sed 's|(.*)||g'`) is replicated exactly
(`desktop_file_display_name`), including its own known, minor
imprecision (a real app name that itself contains a literal `(`
would also get truncated) — a real, if crude, cosmetic-only quirk this
project deliberately doesn't go further than fixing, the same
"preserve a documented real-tool quirk rather than silently diverge"
precedent `0327`/`0328` already established for other cases.

## Implementation

`Command::Export` gained `list_apps`/`list_binaries: bool`. `cmd_export`
now dispatches these two (mutually exclusive with each other and with
`--app`/`--bin`/`--delete`/`--export-label`) before its existing
app/bin dispatch, to `cmd_export_list_apps`/`cmd_export_list_binaries`.
New shared `exported_files_for_box` (a plain directory scan, no
recursion needed — this project's own export directories are always
flat) and `desktop_file_display_name`.

`cmd_export` grew an eighth parameter doing this, tripping clippy's own
`too_many_arguments` — fixed by introducing a small `ExportArgs<'a>`
struct bundling every flag but `box_name` itself, a pure, mechanical
refactor with no behavior change.

## Verified

`cargo build -p ocibox --locked`; `ocibox export --help` renders both
new flags correctly. Five new integration tests in `tests/tests/
ocibox_export.rs` (24 total, 19 pre-existing, all pass unchanged): a
previously-exported app is listed with its default label stripped
back off; an empty/nonexistent export directory lists nothing (a
real, silent success, not an error); a previously-exported binary is
listed by its own real basename; two different boxes' own exports
(different basenames, so no destination collision) never leak into
each other's `--list-binaries` output; and combining either list flag
with the other, or with `--app`/`--bin`, is a clear, immediate error.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (113 test result blocks,
0 failures), `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).

Performance: `ocibox export --list-apps`/`--list-binaries` are
one-shot, offline, read-only commands, not part of any hot-path
benchmark tracked in `docs/benchmarks.md`. No re-benchmark needed.

## Still ahead

`ocibox export`'s own remaining real distrobox flags (`--sudo`,
`--extra-flags`, `--enter-flags`) remain separately-scoped future
candidates, as do `ocibox stop`/`upgrade`/`generate-entry`/`assemble`
(each needing materially bigger architecture work) and `ocivmm`'s own
remaining gaps (a lighter-weight offline `create` success-path
fixture, the HVF/macOS phase-4 blocker).
