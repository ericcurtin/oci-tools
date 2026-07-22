# Design note 0149: `ociman diff`

Status: implemented
Scope: `crates/oci-layer/src/diff.rs` (`Snapshot`/`EntryMeta`/
`EntryKind` now `Serialize`/`Deserialize`, 1 new round-trip test);
`crates/oci-layer/Cargo.toml` (new `serde`/`serde_json` dependencies);
`bin/ociman/src/main.rs` (`Command::Diff`, `cmd_diff`, `DiffReport`,
`BASE_SNAPSHOT_FILENAME`, `cmd_run`'s new base-snapshot-capture call
site, `resolve_container_root`'s signature generalized to also return
the loaded state and take a command-name parameter — `cmd_cp`'s own
two call sites updated, no behavior change there); `tests/tests/
ociman_diff.rs` (new, 5 tests).

## A real, valuable, well-known gap

`docker diff`/`podman diff` — listing every path that differs between
a container's own current filesystem and the base image it was
created from (`A`dded/`C`hanged/`D`eleted) — had no counterpart in
`ociman` at all. A natural fit for this project's own existing
`oci_layer` crate, which already implements exactly this comparison
algorithm for `ociman build`'s own `RUN`/`COPY`/`ADD` commit step.

## A real bug found and fixed *before* committing to the wrong design

The first version of this feature tried to diff a container's current
`rootfs/` directly against `oci_store::ensure_cached`'s own shared,
read-only rootfs-cache directory (the same one `ociman run`'s overlay
path and `ociman build`'s stage population already key off) — building
or reusing it fresh at `diff` time rather than persisting anything new
per container. This looked elegant, but a real, throwaway build
surfaced a genuine correctness bug before it was ever committed: a
stock busybox image's own `/bin/busybox` (an ordinary file the
container never touched at all) showed up as `C` (changed).

Root cause, found by reading `oci_layer::apply`'s own source directly:
it deliberately never restores a tar entry's own original mtime (a
real, already-documented design choice — nothing in this project's
own extraction path ever needed it before now). Two *independent*
extractions of the exact same layer content therefore get *different*
real mtimes for every regular file, purely from being extracted at two
different wall-clock moments. `oci_layer::diff`'s own comparison is
deliberately, and correctly, mtime-sensitive — but only for its actual
intended use, confirmed directly from its own module doc comment:
"every file this module ever compares is either genuinely untouched
since its own most recent real write, or was genuinely rewritten by
something between the two snapshots" — an assumption that holds for
`ociman build`'s own before/after pair (the *same* physical directory,
two points in real time) but not for two *separately extracted copies*
of the same content.

## The fix: persist a real snapshot, don't re-derive one later

`cmd_run` now captures a real `oci_layer::Snapshot` of a plain-
`Extract`-mode container's own `rootfs/` right after every layer has
been extracted and `/etc/hosts` has been written (0147/0148's own
established trick again: capturing *after* both means neither ever
shows up as a spurious diff entry later — confirmed directly, matching
real docker/podman's own hiding of their synthesized hosts/resolv.conf
files from `diff` output too, achieved here by a different but
equally effective mechanism) — serialized via `serde_json` to
`base-snapshot.json` in the container's own bundle directory,
alongside `state.json`/`config.json`. `ociman diff` loads that exact
file back and diffs the container's own *current* `rootfs/` against
it directly — the same physical directory, two points in real time,
exactly the shape `oci_layer::diff` is actually designed for.

This needed `Snapshot`/`EntryMeta`/`EntryKind` to become
`Serialize`/`Deserialize` (a new, small `serde` dependency for
`oci-layer` — already a shared workspace dependency elsewhere, no new
external capability). A new round-trip test (`snapshot_round_trips_
through_json_and_still_diffs_correctly`) confirms a `Snapshot`
serialized and reloaded still diffs identically to the original,
including a real, genuine change still being detected correctly
afterward.

An overlay-mode container gets no snapshot at all (its own `rootfs/`
stays empty on the host's own view for its entire life — see
`rootfs_setup`'s own doc comment — so a snapshot of it would never be
useful); `resolve_container_root` already rejects that case with a
clear error before `cmd_diff` ever needs the file, the same real,
checked-directly gap `ociman cp` (0146) already has.

## A small, useful shared refactor

`resolve_container_root` (previously `cmd_cp`-only) now takes a
`command_name: &str` parameter (for its own error message) and returns
`(PathBuf, PersistedState)` instead of just the root path — `cmd_diff`
needs the loaded state's own `bundle` field too (to locate
`base-snapshot.json`), and there's no reason to load it a second time.
`cmd_cp`'s own two call sites updated trivially; all its own existing
tests still pass unmodified.

## Output format matches real podman exactly, not just approximately

Checked directly (`~/git/podman/cmd/podman/diff/diff.go`): the plain-
text format is `"%s %s" % (Kind, Path)` (`C`/`A`/`D`, `Path` with a
leading `/`) — matches `~/git/moby/vendor/github.com/moby/go-archive/
changes.go`'s own `ChangeType.String()`/`Change.String()` exactly, the
same real differ real podman itself still depends on. The `--json`
shape is real podman's own `ChangesReportJSON`: three separate
`changed`/`added`/`deleted` string arrays (each omitted entirely when
empty, `omitempty`), not one flat `{path, kind}` list.

## Real, automated tests

Five new integration tests in `tests/tests/ociman_diff.rs`: added and
deleted paths reported correctly, and — the actual regression test for
the bug found above — an untouched base-image file (`busybox`) and the
synthesized `/etc/hosts` both confirmed absent from the output; the
same `--json` three-array shape; a container with *no* deliberate
changes at all reports nothing except the runtime's own real,
pre-existing `/dev`/`/proc`/`/sys` mount-point directories (matching
real docker/podman's own behavior for the same reason — these are
genuinely created fresh in the container's own rootfs); an unknown
container is a clear error; the rootless-overlay-rootfs rejection
(written to pass correctly either way depending on whether the test
host happens to support that optimization, same technique
`ociman_cp.rs` already established).

Full `cargo build --workspace --locked`/`cargo test --workspace
--locked` (2 clean runs)/`cargo fmt --all --check`/`cargo clippy
--workspace --all-targets --locked -- -D warnings`/`python3
ci/guards.py`/`cargo deny check`/`bash ci/native-ci.sh` all clean.

## What this doesn't do yet

* **Rootless-overlay-rootfs containers** — same real gap `ociman cp`
  already has (0146): needs real overlayfs-whiteout-aware handling
  (distinguishing a genuine deletion, marked by a character-device
  whiteout entry in the upper layer, from a path simply never having
  existed) this increment doesn't implement.
* **A container created before this feature existed** has no
  `base-snapshot.json` at all — a clear, real error rather than a
  guess.
* **Extended attributes** (`security.capability` included) — same
  gap `oci_layer::diff` already has for `ociman build`'s own use of
  it; nothing in this project's own layer-application path
  extracts/restores them to begin with.
