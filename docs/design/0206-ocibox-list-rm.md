# Design note 0206: `ocibox list`/`ocibox rm`

Status: implemented
Scope: `bin/ocibox/src/main.rs` (`Command::List`/`Command::Rm`,
`cmd_list`, `cmd_rm`, `list_boxes`); `tests/tests/ocibox_list_rm.rs`.

## Continuing milestone 7

0205's own "what this doesn't do yet" named `ocibox list`/`rm` as part
of rounding out the family enough to actually manage what `create`
makes — right now, once a box exists, there was no way to find or
remove it again except by hand-inspecting the filesystem.

## The fix

`ocibox list` (alias `ls`, matching real `distrobox list`'s own
identical alias): reads every `<boxes_root>/*/box.json`, sorted by
name (matching real `distrobox list`'s own stable sort order, checked
directly against its actual source, `pkg/commands/list.go`). A
directory with no readable `box.json` is skipped, not a hard failure
for the whole listing — the same "one broken entry shouldn't hide
every other, otherwise real one" tolerance `oci_bls::scan_entries`
already established for BLS entries. Deliberately narrower than real
`distrobox list`'s own output (real container status/id columns): this
project's own boxes aren't real running containers yet at all (`ocibox
create` only extracts a rootfs and records metadata so far — `ocibox
enter`, still ahead, is what will actually launch one), so there's
nothing more truthful to show yet.

`ocibox rm <NAME>` (`--force` accepted for real CLI compatibility with
`distrobox rm --force`, but changes nothing: this project has no
interactive confirmation prompt to skip in the first place, the same
"nothing to skip" reasoning `create --pull`'s own doc comment already
gives for `--yes`): removes `<boxes_root>/<name>` entirely. A name
that doesn't exist is a clear, real error, matching real `distrobox
rm`'s own identical refusal.

## A real security fix caught while implementing this

`cmd_rm`'s own `name` argument, joined directly onto `boxes_root()`
before ever calling `remove_dir_all`, needed exactly the same
`validate_box_name` check `cmd_create` already applies before it ever
constructs a path from user input — without it, a name containing `/`
or `..` components (e.g. `ocibox rm ../../etc`) would let this
function's own recursive removal reach an arbitrary path *outside*
`boxes_root()` entirely, a real path-traversal hazard, not just a
cosmetic naming inconsistency. Caught by re-reading `cmd_create`'s own
reasoning for validating first — `cmd_rm`'s first draft didn't call
`validate_box_name` at all — and confirmed directly: a real path-
traversal attempt (`ocibox rm ../canary.txt` against a real canary file
placed just outside `boxes_root`) is now rejected as an invalid name
before any real filesystem removal is ever attempted, and the canary
survives untouched.

## Verified by hand

* `list`/`ls` on an empty store: `no boxes`, exit success.
* Three boxes created out of alphabetical order list back sorted by
  name.
* `--json list` reports every persisted field.
* `rm` removes a real box's entire directory (rootfs and record
  alike); `list` afterward confirms it's gone.
* `rm` of an unknown name is a clear error; a path-traversal attempt in
  the name is rejected before touching the filesystem at all.

## Tests

Seven new integration tests in `tests/tests/ocibox_list_rm.rs`,
covering every scenario above including the path-traversal rejection
with a real canary-file survival check. All 4 pre-existing `ocibox
create` tests continue to pass unchanged.

Full `cargo build --workspace --locked`/`cargo test --workspace
--locked` (2 clean runs, 87/87 result blocks — one more than before,
the new test binary)/`cargo fmt --all --check`/`cargo clippy
--workspace --all-targets --locked -- -D warnings`/`python3
ci/guards.py`/`cargo deny check`/`bash ci/native-ci.sh` all clean. No
performance regression (`ociman run --rm`, ~67ms, consistent with
prior measurements — this change touches only `ocibox`'s own code).

## What this doesn't do yet

Actually launching a box (`ocibox enter`, landed in 0207), `ocibox
stop`, X11/Wayland/audio/nvidia passthrough, init-hooks,
additional-package installation, cloning an existing box, and
`rm --all` (landed as its own small follow-up, see the changelog
entry after 0207 in `README.md`)/`--rm-home` are all still ahead.
