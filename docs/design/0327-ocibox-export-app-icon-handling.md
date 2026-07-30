# Design note 0327: `ocibox export --app` icon handling

Status: implemented
Scope: `bin/ocibox/src/main.rs`, `tests/tests/ocibox_export.rs`,
`README.md`.

## What this closes

`0322` implemented `ocibox export --app`'s core `Exec=`-rewriting
mechanism but deliberately left `Icon=` completely untouched, calling
icon search/copy/rewrite out of scope as its own, separate increment
— repeatedly flagged since as the single largest remaining gap in
`ocibox`'s own "still ahead" list (`0321`, `0322`, `0324`, `0325`).
This note closes it.

## Real, checked-directly semantics

Read real `distrobox-export`'s own `export_application` function
directly (`~/git/distrobox/internal/inside-distrobox/assets/
distrobox-export:405-593`) rather than guessed. It runs *inside* the
container (via `distrobox-enter`), so its own paths are prefixed with
`/run/host$HOME` to reach the real host home directory bind-mounted at
that path; `ocibox` runs directly on the *host* (its own established,
`0207`-era design choice — it never enters the box's own namespace to
do its work at all), so the equivalent logic here reads straight from
`<box>/rootfs/...` on the host filesystem and writes straight to the
real, unprefixed `$HOME` — no `/run/host` indirection needed anywhere.

The real algorithm, ported faithfully:

1. **Resolve `Icon=` to real file(s).** An already-absolute value is
   used as-is only if it genuinely exists (inside the box's own
   rootfs, here); a bare name is searched for (case-insensitive
   substring, recursively) under three canonical directories:
   `usr/share/icons`, `usr/share/pixmaps`,
   `var/lib/flatpak/exports/share/icons`.
2. **Map each found file to its real host destination.** A path under
   `usr/share/` or `var/lib/flatpak/exports/share/` maps to the
   equivalent path under `.local/share/`, with any `pixmaps` path
   component additionally renamed to `icons` (`.local/share/pixmaps`
   isn't a real XDG icon-theme search location at all, unlike
   `.local/share/icons` — checked directly, this is exactly why real
   distrobox itself does this rename). A path outside both canonical
   prefixes (a real, if rare, vendor-specific icon location) falls
   back to a flat `.local/share/icons/<basename>` destination instead.
3. **Copy, skipping an already-present destination** (real
   distrobox's own identical `[ ! -e dest ]` "don't clobber" check).
4. **Rewrite `Icon=` only when genuinely necessary.** A bare name is
   left completely untouched — it resolves via the icon theme's own
   normal lookup once its file exists at the mapped destination. An
   already-absolute, non-canonical hard path (case 2's fallback) *must*
   be rewritten to the new absolute host path, since the original path
   only ever existed inside the box's own rootfs. An already-absolute
   path under the canonical `/usr/share/` prefix specifically gets that
   prefix rewritten to `$HOME/.local/share/`, matching real
   distrobox's own identical (if narrow) `sed` rule — real distrobox's
   own script only ever has this one specific `Icon=/usr/share/...`
   `sed`, never a matching one for the flatpak-prefixed case, so an
   already-canonical *flatpak* absolute path is left unrewritten too,
   a real, minor gap in real distrobox itself this project deliberately
   preserves rather than silently "fixing" beyond what the real tool
   does.

`--delete` mirrors this exactly, re-resolving the same icon(s) against
the box's own still-present rootfs and removing whichever of their
computed host destinations still exist — tolerant of one already
being gone, matching real distrobox's own identical unconditional-but-
tolerant `rm -rf` there. An icon file has no marker/safety-check of its
own (unlike the `.desktop` file itself); this matches real distrobox
exactly, which has none either.

## Implementation

New: `ICON_SEARCH_DIRS`, `IconResolution` (an enum distinguishing the
"bare name, 0+ real matches" case from "hard path, exactly one match"
— the same two branches real distrobox's own shell logic has),
`resolve_icon`/`resolve_icon_files`, `find_icon_files_recursive` (a
plain recursive `std::fs::read_dir` walk, no new dependency),
`icon_export_destination` (the path-mapping rule above),
`export_icon_file`/`remove_exported_icon_file`, and
`desktop_file_icon_value`. `rewrite_desktop_file` gained two new
parameters (`icon_rewrite: Option<&str>`, `home: &Path`) to apply the
`Icon=` rewrite rules above. `cmd_export_app` now resolves/copies (or,
on `--delete`, removes) each desktop file's own icon(s) alongside the
existing `.desktop`-file logic, computing `$HOME` once via a new,
shared `home_dir()` helper (`default_export_path`/
`default_app_export_path` also refactored onto it, a pure, verified-
unchanged move).

One real, minor, honestly-documented behavior change: `cmd_export_app`
now needs `$HOME` set even when `--export-path` is given explicitly
(previously only needed for the *default* export path) — icon
destinations are always computed from `$HOME` regardless, matching
real distrobox's own identical dependency on a real `host_home`.

## Verified

`cargo build -p ocibox --locked`. Five new integration tests in
`tests/tests/ocibox_export.rs` (15 total, 10 pre-existing, all
unchanged and passing): a themed `usr/share/icons/hicolor/48x48/apps/`
bare-name icon copied to the identical relative path under
`$HOME/.local/share/icons/...` with `Icon=` left untouched; a
`usr/share/pixmaps/` bare-name icon copied to `$HOME/.local/share/
icons/` (not `.../pixmaps/`); a non-canonical absolute `Icon=` path
(`/opt/myapp/icon.png`) copied to a flat `$HOME/.local/share/icons/
<basename>` destination with `Icon=` genuinely rewritten to that new
path; a canonical absolute `Icon=/usr/share/pixmaps/...` path rewritten
to `Icon=$HOME/.local/share/icons/...`; and `--delete` also removing a
previously-copied icon. The one pre-existing test whose own doc
comment claimed icon handling was unimplemented
(`export_app_writes_a_rewritten_desktop_file`) still passes unchanged
(its own fixture's bare icon name has no matching file anywhere in the
synthetic test box, so nothing was ever found to copy — a genuine,
different reason for the same observed "untouched" result), with its
doc comment corrected to say so.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (113 test result blocks,
0 failures), `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).

Performance: `ocibox export --app` is a one-shot, offline (once icons
are already present inside the box's own rootfs) command, not part of
any hot-path benchmark tracked in `docs/benchmarks.md`. No
re-benchmark needed.

## Still ahead

`ocibox`'s own remaining gaps — `stop` (needs a persistent background
container, a materially bigger architecture change), `upgrade` (needs
real in-container multi-distro package-manager dispatch),
`generate-entry`/`assemble` (batch/manifest-driven creation),
`export --list-apps`/`--list-binaries`/`--sudo`/`--export-label`/
`--extra-flags`/`--enter-flags` — all remain separately-scoped future
candidates. `ocivmm`'s own remaining gaps (a lighter-weight offline
`create` success-path fixture, the HVF/macOS phase-4 blocker) are
unaffected by this note.
