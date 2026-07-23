# Design note 0211: `ocibox ephemeral`

Status: implemented
Scope: `bin/ocibox/src/main.rs` (`Command::Ephemeral`, `cmd_ephemeral`,
`random_box_name`, `unique_random_box_name`; refactors `cmd_create` ->
`create_box` and `cmd_enter` -> `enter_and_get_exit_code` so both are
reusable without their own final `println!`/`std::process::exit`);
`tests/tests/ocibox_ephemeral.rs`.

## Rounding out the `ocibox` family further

`create`/`list`/`rm`/`rm --all`/`enter` (0205-0208) cover managing and
launching a named, persistent box. Real `distrobox` also has
`ephemeral`: a disposable box under a random name, entered once, then
always removed — confirmed as a real, registered CLI subcommand
(`~/git/distrobox/internal/cli/ephemeral.go`, not just the underlying
`pkg/commands/ephemeral.go` it wraps), not a bash-only legacy leftover.

## A pure composition, no new launch code

`cmd_ephemeral` is exactly three calls to already-existing, already-
tested primitives: `create_box` (the refactored guts of `cmd_create`,
now with no final `println!` of its own — an ephemeral box's own
generated name isn't worth a stdout line the way `create`'s own
user-chosen `--name` is, matching real `distrobox ephemeral`'s own
"drop straight into the shell" output), `enter_and_get_exit_code` (the
refactored guts of `cmd_enter`, now *returning* the real exit code
instead of calling `std::process::exit` itself — needed so cleanup can
still run afterward), and `remove_one_box` (already shared with `ocibox
rm`). Zero new namespace/mount/launch code was written for this
increment at all.

## Always cleans up, even on failure

The box is removed after `enter_and_get_exit_code` returns, whether it
succeeded, returned a nonzero exit code, or failed outright (e.g. a
spec-build error) — matching real `distrobox ephemeral`'s own
`defer`-based cleanup exactly (`pkg/commands/ephemeral.go`'s own
`defer func() { ... rmCmd.Execute(...) }()`, always running regardless
of how `enterCmd.Execute` returned). A cleanup failure is only ever
printed as a warning; it never replaces or masks the real result
(the command's own success/failure/exit code), matching that same real
behavior (`c.printer.PrintWarningln` on an `rmErr`, not a hard failure
of the whole `ephemeral` command).

## Random name generation, dependency-free

Real `distrobox ephemeral` generates `"distrobox-" + 10 random
alphanumeric chars"`, retried up to 10 times on a real collision
(`ephemeralMaxNameGenAttempts`, `makeRandomName`). This project has no
`rand` crate anywhere in the workspace and didn't need to add one:
`random_box_name` hashes the real current time, this process's own
pid, and an attempt counter (the exact same dependency-free technique
`ociman`'s own `short_id` already uses for container IDs) into
`"ocibox-<12 hex chars>"`, and `unique_random_box_name` retries it up
to the same 10 attempts real distrobox uses if a real collision is
ever found.

## Verified by hand

* `ocibox ephemeral --image ... -- /bin/echo hello`: prints `hello`,
  exits 0, and the box is completely gone afterward (`ocibox list`
  reports `no boxes`).
* A nonzero exit inside the ephemeral box (`/bin/sh -c 'exit 5'`)
  becomes `ocibox ephemeral`'s own real exit code, and the box is
  still removed.
* No `COMMAND` given falls back to a default shell (same detection
  `ocibox enter` already established).
* An unresolvable `--image` fails clearly and leaves no half-created
  box directory behind at all (`create_box`'s own existing cleanup-
  on-failure logic, reused verbatim).

## Tests

Five new integration tests in `tests/tests/ocibox_ephemeral.rs`
(success + cleanup, nonzero exit + cleanup, default shell, two
invocations never sharing rootfs state with each other, and a failed
create leaving nothing behind). All 24 pre-existing `ocibox`
integration tests (`create`/`list_rm`/`enter`) continue to pass
completely unmodified, confirming the `cmd_create`/`cmd_enter`
refactors are pure extractions with no behavior change.

Also fixed a real, if minor, pre-existing formatting slip found while
re-running `cargo fmt --all --check` this turn: one array literal in
`tests/tests/ociman_build.rs` (added in 0210) had been left in a
non-canonical multi-line form that `cargo fmt --all --check` on a
clean `origin/main` checkout actually flags — confirmed by stashing
this increment's own changes and re-running the check against the
untouched, already-pushed commit. Folded into this same commit as a
trivial, unrelated-in-content but appropriately-bundled formatting fix.

Full `cargo build --workspace --locked`/`cargo test --workspace
--locked` (2 clean runs, 89/89 result blocks — one more than before,
the new `ocibox_ephemeral.rs` test binary)/`cargo fmt --all --check`/
`cargo clippy --workspace --all-targets --locked -- -D warnings`/
`python3 ci/guards.py`/`cargo deny check`/`bash ci/native-ci.sh` all
clean. No performance regression (`ociman run --rm`, ~69ms, consistent
with prior measurements — this change touches only `ocibox`'s own
code).

## What this doesn't do yet

`ocibox stop`, `ocibox upgrade` (real distrobox's own version runs an
in-container package-manager upgrade script against an already-
running, persistent container — not a good fit for this project's own
single-shot `enter` model yet), X11/Wayland/audio/nvidia passthrough,
and `ocibox export` (a real, separate, in-container bash tool in
distrobox's own asset set, not a host-side Go/Rust command at all) all
remain out of scope.
