# Design note 0486: `ocibox export --enter-flags`

Status: implemented
Scope: `bin/ocibox/src/main.rs`, `tests/tests/ocibox_export.rs`.

## What this closes

Real `distrobox export --enter-flags` had no equivalent in `ocibox
export` — flagged in `0330`'s own "still ahead" note, at the time
correctly deferred because "`ocibox enter` itself has no options of
its own yet for such a flag to filter/forward at all." That specific
blocking condition changed in `0468`, which gave `ocibox enter` a
real, meaningful `--clean-path`/`-c` flag — so `--enter-flags` now has
something genuine to forward. Re-opening a previously-deferred item
based on new evidence that its own original blocker no longer applies
matches this project's own established precedent (`0475`'s identical
correction of `0388`'s prior reasoning).

## Real, checked-directly semantics

`~/git/distrobox/internal/inside-distrobox/assets/distrobox-
export:179-181,243-264,291,332`:

- Lines 179-181: `-nf | --enter-flags) enter_flags="$2" ...` — a
  plain string value.
- Lines 243-264: filters `enter_flags`, stripping any given
  `--root`/`-r` or `--name`/`-n` token (each with a printed warning:
  the export wrapper already sets those two automatically), keeping
  everything else verbatim.
- Line 291 (`--app` mode): `container_command_prefix="distrobox
  enter${rootful:+ ...} -n ${container_name}${enter_flags:+
  ${enter_flags}} --${sudo_prefix:+ ...} "` — inserted between the
  container name and the `--` separator, later prepended directly
  onto the original `Exec=` line's own content (line 567).
- Line 332 (`--bin` mode): `exec distrobox enter ${rootful} -n
  ${container_name} ${enter_flags} -- ${sudo_prefix}
  ${container_command_suffix}` — the identical shape, inside the
  generated wrapper script.

## Implementation

- `Command::Export` gains `enter_flags: Option<String>`
  (`--enter-flags`, `allow_hyphen_values = true`, the same reasoning
  `--extra-flags` already established for real flag-shaped values).
  Real distrobox's own short form is the literal two-character,
  single-dash token `-nf` — a plain shell-argument string comparison
  in its own hand-rolled parser, not a getopt-style single-char short
  flag at all. Clap's own `short` mechanism only accepts one
  character, so that exact spelling has no faithful equivalent —
  long-only here rather than inventing a subtly wrong single-
  character stand-in.
- Real distrobox's own "filter out `--root`/`-r`/`--name`/`-n`" step
  has no honest equivalent to replicate: this project's own `ocibox
  enter` has neither flag at all (its own box name is a plain
  positional, never a `--name`/`-n` flag; this project has no
  rootful/rootless distinction to have a `--root`/`-r` flag for in
  the first place) — a real, honest scope simplification, not an
  oversight, so `--enter-flags`'s value is inserted verbatim, no
  filtering needed.
- `ExportArgs`/`cmd_export`/`cmd_export_bin`/`cmd_export_app`/
  `rewrite_desktop_file` all gained a threaded `enter_flags:
  Option<&str>` parameter. `cmd_export_bin`'s wrapper-script template
  and `rewrite_desktop_file`'s rewritten `Exec=` line both insert it
  identically: between the box name and the `--` separator (`exec
  ocibox enter {box_name}{enter_flags} -- ...`), matching real
  distrobox's own identical shape for both modes.

## Tests

Three new integration tests in `tests/tests/ocibox_export.rs`:
`export_bin_enter_flags_are_inserted_between_the_box_name_and_the_
separator`, `export_bin_enter_flags_and_extra_flags_compose_
correctly` (proving `--enter-flags` lands before the `--` separator
and `--extra-flags` after it, in the same single wrapper script),
`export_app_enter_flags_are_inserted_between_the_box_name_and_the_
separator` (the `--app` mode's own identical insertion point in the
rewritten `Exec=` line). All 30 tests in the file pass (27 prior + 3
new).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (122 test-result
blocks, 0 failures on the first attempt), `python3 ci/guards.py`
(clean), `cargo deny check` (clean), `bash ci/native-ci.sh` (clean,
122/122 on the first attempt), `bash ci/build-deb.sh` (clean, real
`dpkg -i`/`--version`/`dpkg -r` round trip on the first attempt). No
benchmark re-run needed: `ocibox export` is not exercised by `ci/
bench.sh` at all.

## Deliberately still out of scope

`ocibox export --sudo`/`-S` — real, upstream-confirmed
(`distrobox-export`'s own live in-container runtime probing at export
time), but still a genuine architecture mismatch this project has no
equivalent of; correctly still deferred, not attempted here.
