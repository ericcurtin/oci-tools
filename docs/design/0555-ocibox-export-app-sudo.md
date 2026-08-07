# Design note 0555: `ocibox export --app --sudo`/`-S`

Status: implemented
Scope: `bin/ocibox/src/main.rs`, `tests/tests/ocibox_export.rs`.

## What this closes

`0525` added `--sudo`/`-S` to `ocibox export --bin`, but deliberately
rejected the combination with `--app` outright (a clear, immediate
`bail!`, not a silent no-op), explicitly deferring "wiring `--sudo`
into `--app`'s own generated desktop entry" to a future increment.
This closes that gap: `ocibox export --app --sudo` now writes a
rewritten `.desktop` file whose `Exec=` line runs the application
through `sudo` inside the box, exactly mirroring `--bin`'s own
already-shipped behavior.

## Real, checked-directly confirmation

- `~/git/distrobox/internal/inside-distrobox/assets/
  distrobox-export:270-289`: builds `sudo_prefix` (`sudo -S`/`sudo`/
  `doas`/`su-exec root`) -- the same detection/priority logic `0525`
  already cited and deliberately simplified for `--bin`.
- `~/git/distrobox/internal/inside-distrobox/assets/
  distrobox-export:291`: `container_command_prefix="...--${sudo_
  prefix:+ ${sudo_prefix}} "` -- **the one shared prefix string used
  by both `--bin`'s own wrapper template and `--app`'s own desktop-
  entry rewrite**, proving this is a real, shared mechanism, not a
  `--bin`-only one.
- `~/git/distrobox/internal/inside-distrobox/assets/
  distrobox-export:567`: `sed "s|^Exec=\(.*\)|Exec=${container_
  command_prefix}\1|g" "${desktop_file}"` -- the exact line that
  bakes `sudo_prefix` (via `container_command_prefix`) into the
  exported `.desktop` file's own `Exec=` line. This is the live
  consumer proving `--sudo` genuinely applies to `--app` too, not
  dead code.

This project's own `--app` export is entirely host-side and static
(`find_desktop_files`/`rewrite_desktop_file` -- a plain text rewrite,
never a live command run inside the box), unlike real distrobox's own
script, which runs *from inside* the box and can genuinely probe live
capability/`$PATH`. The same real, deliberate simplification `0525`
already applied to `--bin` applies here identically: only plain
`/usr/bin/sudo` is checked for, statically, inside the box's own
rootfs -- `doas`/`su-exec` detection and the passwordless-`sudo -S`
probe both need live execution this project's export model has never
had, and remain a real, separate, deliberately deferred gap (shared
with `--bin`'s own identical one, not a new one introduced here).

A box with no `/usr/bin/sudo` at all is a real, immediate, clear
error at export time -- the same "fail clearly and early" convention
`--bin`'s own identical check already established.

## Why this is narrow (unlike the `ocirun run/create --config`
candidate considered and set aside for this slot)

Entirely contained to one command's own implementation
(`cmd_export_app`/`rewrite_desktop_file`), reusing the exact static
`rootfs.join("usr/bin/sudo").is_file()` check `cmd_export_bin` already
established. No container lifecycle, no persisted state, no reload
sites -- a pure "read the box's rootfs statically, rewrite one
`Exec=` line in a `.desktop` file" operation.

## Implementation

- `cmd_export`'s own `(Some(_), None) if sudo => bail!(...)` rejection
  removed; `sudo` is now threaded straight into `cmd_export_app`.
- `cmd_export_app` gains a `sudo: bool` parameter (now 8 arguments,
  `#[allow(clippy::too_many_arguments)]` added, matching the same
  convention `cmd_export_bin`/`cmd_import` etc. already use): when
  true, checks `<box>/rootfs/usr/bin/sudo` exists (erroring clearly
  if not, before writing anything at all) -- checked only on the
  non-`--delete` path, exactly mirroring `cmd_export_bin`'s own
  identical choice to only check (and only ever consume) `sudo` when
  actually writing a wrapper, not when removing one.
- `rewrite_desktop_file` gains a `sudo: bool` parameter: when true,
  inserts a literal `sudo ` right after the `--` separator and before
  the rest of the original `Exec=` value -- the same position real
  distrobox's own `container_command_prefix` places `sudo_prefix` in,
  and the same position `--bin`'s own template already uses for it.
- `Command::Export::sudo`'s own doc comment updated to describe the
  flag as applying to both `--bin` and `--app` alike, with the
  `doas`/`su-exec` simplification now described once (it was never
  `--bin`-specific to begin with).

## Tests

Two new integration tests in `tests/tests/ocibox_export.rs`, mirroring
`0525`'s own `--bin` test pair exactly:
`export_app_sudo_flag_prefixes_the_rewritten_exec_line_with_sudo` (a
real `usr/bin/sudo` seeded into the box's rootfs, verifying the
rewritten `Exec=` line reads `Exec=ocibox enter testbox -- sudo
/usr/bin/myapp --flag`) and
`export_app_sudo_without_sudo_installed_is_a_clear_error` (verifies
the clear error and that no `.desktop` file is written at all). The
old `export_app_and_sudo_together_is_a_clear_error` test (which
asserted the now-removed rejection) is replaced by these two.

Manually verified end-to-end by hand beyond the automated tests: built
a real `scratch`-based image (via `ociman build`) containing busybox,
a fake `/usr/bin/sudo`, and a real `.desktop` file, created a box from
it, and ran `ocibox export --box sudobox --app "My App" --sudo`,
confirming the written `.desktop` file's `Exec=` line reads exactly
`Exec=ocibox enter sudobox -- sudo /usr/bin/myapp --flag`; separately
confirmed a box without `/usr/bin/sudo` produces the clear error and
writes nothing.

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (128
test-result blocks, all passing -- no new test file added, so the
block count is unchanged from `0554`), `python3 ci/guards.py` (clean),
`cargo deny check` (clean), `bash ci/native-ci.sh` (clean on the first
attempt), `bash ci/build-deb.sh` (clean on the first attempt, real
`dpkg -i`/`--version`/`dpkg -r` round trip). An export-time-only
feature, not part of any launch/run hot path -- no `ci/bench.sh`
rerun needed (same reasoning `0525` already gave).

## Deliberately still out of scope

`doas`/`su-exec` detection and the passwordless-`sudo -S` capability
probe remain open candidates for both `--bin` and `--app` alike (both
need the same live-probe machinery this project's static export model
has never had) -- unchanged from `0525`'s own still-open list.
