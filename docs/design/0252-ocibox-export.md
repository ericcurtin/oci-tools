# Design note 0252: `ocibox export --bin`

Status: implemented
Scope: `bin/ocibox/src/main.rs`, `tests/tests/ocibox_export.rs`.

## What real `distrobox export` does

Real `distrobox export` (checked directly against its own actual shell
implementation, `~/git/distrobox/internal/inside-distrobox/assets/
distrobox-export` — no Go rewrite of it exists yet, unlike most of the
rest of real distrobox) has two modes: `--app` (copies a `.desktop`
file plus its icons onto the host, prefixing the launch command with
`distrobox enter`) and `--bin` (writes a small wrapper script that
runs a container-side binary via `distrobox enter` from the host).
This slice implements only `--bin` — `--app` needs real desktop-file
parsing and icon extraction/copying this project has none of yet, a
materially bigger feature left honestly for later, matching this
project's own established "narrow first slice, document the rest"
pattern (`ocibox create`/`enter`/`ephemeral` before it, each scoped
down from real distrobox's own richer feature set the same way).

## One real, necessary divergence

Real `distrobox export` is meant to be run **from inside** the
container: it detects which box it's running in via its own
`$CONTAINER_ID` env var (set by real `distrobox enter`'s own
persistent session) and writes the wrapper onto the host's `$HOME`
(bind-mounted into the container already). `ocibox enter` doesn't have
that infrastructure — no persistent keeper process a shell session
inside a box could report itself to (`Command::Enter`'s own doc
comment already documents this same gap: each `enter` is its own
independent, foreground container).

So `ocibox export` instead runs from the **host** and takes an
explicit `--box <name>` naming which box to route the wrapper's own
invocations through. A real, honestly-documented divergence — not a
silent behavior change, and not a loss of anything the real tool's own
`--bin` mode fundamentally needs (the box name is just as necessary an
input either way, only the *source* of it differs).

## The wrapper script

```sh
#!/bin/sh
# ocibox_binary
# box: <box_name>
exec ocibox enter <box_name> -- '<bin>' "$@"
```

Matches real `distrobox-export`'s own `generate_script` template
directly (the `distrobox_binary` marker comment, the single-quoted
binary path, `"$@"` forwarding every argument through unmodified) —
just namespaced (`ocibox_binary`) and simplified: no `$CONTAINER_ID`
branch (nothing here ever runs *inside* a box the way the real
template's own three-way branch anticipates), no `--rootful`/`--sudo`
concepts (neither exists anywhere in this project).

`--delete` reverses it, with the same real safety check real
`distrobox export --delete` itself does: refuse to touch a
destination file that doesn't actually contain the marker comment,
so a stray `--bin`/`--export-path` combination can never delete an
unrelated file by mistake (or, symmetrically, a real `distrobox`
export sharing the same destination directory).

## Verified

Integration (`tests/tests/ocibox_export.rs`), against a real,
already-`create`d box:

- The wrapper is written, executable, contains the marker/box
  name/binary path, and — the real proof, not just a plausible-looking
  file — actually running it (a real `sh` invoking the generated
  `exec ocibox enter ...` line) launches the exported binary inside
  the real box and forwards its arguments/output correctly.
- A missing binary inside the box's own rootfs is a clear error,
  leaving no wrapper behind.
- An unknown `--box` is a clear error.
- `--delete` removes a genuine wrapper; a foreign, non-exported file
  at the same destination is refused and survives completely
  untouched.
- A non-absolute `--bin` is rejected before touching anything.

Full workspace: `cargo build`/`test --workspace`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`python3 ci/guards.py`, `cargo deny check`, `ci/native-ci.sh`,
`ci/build-deb.sh`.

## Still ahead

`ocibox`'s own remaining milestone-7 gaps: `--app` desktop-entry
export, and `stop`/`upgrade` (real `distrobox upgrade` — checked
directly against `~/git/distrobox/pkg/commands/upgrade.go` — actually
runs the guest's own package manager via a multi-distro `entrypoint
--upgrade` dispatch, a materially bigger feature than an image
re-pull, still ahead in full).
