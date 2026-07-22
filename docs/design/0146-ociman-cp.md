# Design note 0146: `ociman cp`

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Cp`, `cmd_cp`,
`parse_user_input`, `resolve_container_root`, `resolve_container_path`,
`copy_cp_path`); `bin/ociman/src/build.rs` (`copy_path_recursive` made
`pub(crate)`, no behavior change); `tests/tests/ociman_cp.rs` (new, 9
tests).

## A real, commonly-used gap

`docker cp`/`podman cp` — copying files/directories between the host
and a container's own storage, running or stopped — had no
counterpart in `ociman` at all.

## `[CONTAINER:]PATH` parsing, ported exactly

`parse_user_input` is a direct, checked-against port of real podman's
own `parseUserInput` (`~/git/podman/pkg/copy/parse.go`): a bare path
starting with `.` or `/` is never a container reference (this
project's own Linux-only target collapses podman's own extra
`filepath.IsAbs` Windows-drive-letter check into the same "starts with
`/`" test); otherwise, everything up to the first `:` names a
container, purely syntactically — whether that name actually resolves
to a real container is `resolve_container_root`'s own separate,
later check, matching real podman's own `containerMustExist` split
exactly.

## A real, checked-directly discovery made *while building this feature*: two rootfs layouts, only one of which `cp` can read

`ociman`'s own containers use one of two layouts (`rootfs_setup`,
`docs/design/0108`-`0110`): a plain, direct layer extraction into the
container's own `rootfs/` directory (`RootfsSetup::Extract`, the
always-correct fallback), or — when the host supports it — a real
rootless overlay mount (`RootfsSetup::Overlay`) applied *inside* the
container's own private mount namespace, with `rootfs/` itself left
empty on the host's own view and a private per-container `upper/`
directory holding whatever the container itself actually wrote.

This was empirically confirmed while building this exact feature: a
real running container (`echo hi > /marker`, using this dev host's own
real, working overlay setup) left `upper/marker` on disk, not
`rootfs/marker` — the file the `PersistedState::rootfs` field itself
points at stayed genuinely empty. Correctly resolving a container's
own real, current, *merged* view for an overlay-mode container would
need real overlayfs-whiteout/opaque-directory-aware layer merging;
not implemented in this increment. `resolve_container_root` instead
detects an overlay-mode container by its own bundle directory's
`upper/` subdirectory (an unconditional part of `rootfs_setup::
prepare_overlay`'s own layout) and returns a clear, real error rather
than attempting a plausible-looking but silently incomplete copy —
matching this project's own already-established "loud error over
silently-wrong behavior" convention exactly.

## Reusing `ociman build`'s own `copy_path_recursive`, unchanged

Rather than writing a second recursive-copy implementation, `build.rs`'s
own `copy_path_recursive` (already extensively used and tested for
`COPY`/`ADD`) is now `pub(crate)` and called directly from `cmd_cp`
with `chmod`/`chown`/`ignore` all `None` — a real, direct instance of
this project's own "share as much Rust code as possible" design
pillar, not a new capability of its own.

## Directory-merge semantics fall out for free; only one real conflict needed `--overwrite`

Matching real `docker cp`/`podman cp`'s own documented core behavior
(not every edge case — see "what this doesn't do yet"): copying a
*directory* onto an already-existing destination directory needs no
special-casing at all — `copy_path_recursive` already walks the
source's own entries and joins each under the destination, which *is*
"copied into the directory" whether or not the destination existed
before. The one real conflict `--overwrite` governs: a source
*directory* landing on a destination that already exists as a
*non*-directory at that exact literal path (removed first, only with
`--overwrite`). A source *file* landing on an already-existing
destination *directory* is not a conflict at all — `copy_cp_path`
redirects it to land inside that directory under its own basename,
matching the documented behavior for that case too.

## Real, automated tests

Nine new integration tests in `tests/tests/ociman_cp.rs`, covering:
a single file both directions; a file landing inside an existing
container directory under its own basename; a directory copied
recursively (including a nested subdirectory) both directions, plus
copying again to confirm a second copy *merges* into the existing
destination rather than erroring or double-nesting; a `..` path
component being a clear, real error; container-to-container being a
clear, real error; neither side naming a container being a clear
error; the one real `--overwrite` conflict, both without (clear error,
destination untouched) and with (destination replaced) the flag; and
the rootless-overlay-rootfs rejection, written to pass correctly
either way depending on whether this particular test host happens to
support that optimization (checked directly via the same `upper/`
marker `resolve_container_root` itself uses, rather than assuming one
outcome).

Every test forces `.rootless-overlay-supported` to `false` up front
(reusing the pre-existing cached-probe marker file mechanism directly,
see `rootfs_setup::rootless_overlay_supported_cached`'s own doc
comment) except the one test that specifically exercises the overlay
rejection path, giving every other test a deterministic, host-
independent rootfs layout to test against.

Full `cargo build --workspace --locked`/`cargo test --workspace
--locked` (2 clean runs)/`cargo fmt --all --check`/`cargo clippy
--workspace --all-targets --locked -- -D warnings`/`python3
ci/guards.py`/`cargo deny check`/`bash ci/native-ci.sh` all clean.

## What this doesn't do yet

* **Container-to-container copying** — real `podman cp` supports it;
  this increment only covers the far more common "one side is the
  host" case. Copying through the host (two separate `ociman cp`
  invocations) is always available as a workaround.
* **Rootless-overlay-rootfs containers** — see above; needs real
  overlayfs-whiteout-aware layer merging to resolve correctly. Falls
  back to a clear, real error rather than a silently incomplete copy.
  Since a container's rootfs layout is decided once, automatically,
  at `run` time (`rootfs_setup::decide`, degrading gracefully whenever
  the environment doesn't support the optimization), this is a real,
  environment-dependent gap for `cp` specifically until closed.
* **`-` (stdin/stdout streaming)** — real `docker cp`/`podman cp`
  support `-` as either path to stream a tar archive from stdin or to
  stdout; not implemented here.
* Docker/podman's own more exotic path-resolution edge cases (e.g. a
  source directory named with a trailing `/.` specifically requesting
  "copy this directory's own contents, not the directory itself, even
  when the destination doesn't exist yet", or re-resolving a
  destination's own basename through a real symlink) aren't
  replicated — this increment matches the commonly-documented,
  commonly-relied-upon behavior, not every corner of the real
  implementations' own source.
* No `--pause` (real podman's own default-`true`, currently-a-no-op-
  even-in-real-podman flag — see its own `cpFlags`' `"Deprecated"`
  help text, not a real gap at all).
