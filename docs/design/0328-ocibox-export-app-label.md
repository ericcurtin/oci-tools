# Design note 0328: `ocibox export --app --export-label`

Status: implemented
Scope: `bin/ocibox/src/main.rs`, `tests/tests/ocibox_export.rs`,
`README.md`.

## What this closes

Real `distrobox export`'s own `--export-label`/`-el` flag (appends a
label to an exported application's own `Name=` line, defaulting to
`" (on <container_name>)"`) had no equivalent in `ocibox export --app`
at all — flagged in the previous survey (`0327`) as one of the small,
remaining `export` flags real distrobox has that this project didn't.

## Real, checked-directly semantics

Read real `distrobox-export`'s own source directly
(`~/git/distrobox/internal/inside-distrobox/assets/distrobox-
export:38,158-160,297-304,571`) rather than guessed:

- Not given at all: `exported_app_label=" (on ${container_name})"`.
- The literal value `none`: disables the label entirely (empty
  string).
- Any other value: used verbatim, with one leading space prepended
  (`" ${exported_app_label}"`), so the exported file reads `NAME
  LABEL`, not `NAMELABEL`.
- Applied via `sed "s|Name.*|&${exported_app_label}|g"` — this
  unanchored pattern matches (and appends the label to) *any* line
  merely **containing** the substring `Name` anywhere at all, not just
  a line that starts with it. This means real distrobox's own script
  would also append the label to a `GenericName=` line (since `Name`
  is a substring of `GenericName`) or even a `Comment=` line that
  happens to mention the word "Name" in its own free-text value — a
  real, crude quirk of the unanchored regex, not a deliberate design
  choice (nothing in the real tool's own docs or comments suggests
  this is intentional).

This project's own implementation deliberately narrows that one
specific point: the label is only ever appended to a line that
genuinely **starts with** `Name` (covering both the bare `Name=` key
and a localized `Name[xx]=` one — the same real intent real
distrobox's own rule has, just without its own over-matching side
effect that could corrupt an unrelated `GenericName=`/`Comment=` line).
Every other real behavior (the three-way default rule itself, only
ever affecting `--app` since `--bin` has no `Name=` line to append to
at all) is matched exactly.

## Implementation

`Command::Export` gained `export_label: Option<String>` (`--export-
label`, no `-el` alias — this project's own established convention
here, matching `--export-path`'s own pre-existing lack of a `-ep`
alias, doesn't replicate real distrobox's own multi-character
short-flag shapes, which clap has no native equivalent of anyway).
`cmd_export`/`cmd_export_app` thread it through; a new
`resolve_export_label(export_label, box_name) -> String` implements
the three-way default rule above. `rewrite_desktop_file` gained a
`label: &str` parameter, appending it to a line starting with `Name`
(and only that line) if `label` is non-empty.

## Verified

`cargo build -p ocibox --locked`; `ocibox export --help` renders the
new flag correctly. Four new integration tests in `tests/tests/
ocibox_export.rs` (19 total, 15 pre-existing, all pass unchanged):
default label appends `" (on testbox)"`; `--export-label none`
disables it entirely (`Name=` stays exactly as it was); a custom value
is appended verbatim; and a dedicated test with a `GenericName=`/
`Comment=` line each merely containing the substring "Name" confirms
neither is touched, only the genuine `Name=` line gets the label.

Also fixed, while in this area: `Command::Export`'s own top-level doc
comment still claimed (predating `0327`) that icon handling was
unimplemented — a stale doc comment `0327` itself missed updating,
corrected here alongside this note's own change.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (113 test result blocks,
0 failures), `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).

Performance: `ocibox export --app` is a one-shot, offline command, not
part of any hot-path benchmark tracked in `docs/benchmarks.md`. No
re-benchmark needed.

## Still ahead

`ocibox export`'s own remaining real distrobox flags (`--list-apps`,
`--list-binaries`, `--sudo`, `--extra-flags`, `--enter-flags`) remain
separately-scoped future candidates, as do `ocibox stop`/`upgrade`/
`generate-entry`/`assemble` (each needing materially bigger
architecture work) and `ocivmm`'s own remaining gaps (a lighter-weight
offline `create` success-path fixture, the HVF/macOS phase-4 blocker).
