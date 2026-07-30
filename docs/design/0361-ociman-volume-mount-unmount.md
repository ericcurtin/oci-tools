# Design note 0361: `ociman volume mount`/`volume unmount`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_volume.rs`,
`README.md`.

## What this closes

`VolumeCommand` (`0173`'s own "local directory" volume driver) had
`create`/`ls`/`inspect`/`rm`/`rename`/`prune`/`exists`/`export`/
`import` — missing real `podman volume mount`/`volume unmount`
(`podman volume reload`, the one remaining real subcommand, is
plugin-driver-only, correctly out of scope: this project has no
pluggable volume-driver concept at all, matching `ContainerCommand`/
`ImageCommand::Prune`'s own already-established `--filter`/scope
deferrals).

## Real, checked-directly semantics — and a genuine, deliberate divergence

Read `~/git/podman/cmd/podman/volumes/mount.go`/`unmount.go` directly,
then confirmed against a real installed `podman 4.9.3` both rootless
and rootful:

```
$ podman volume mount myvol            # rootless
Error: cannot run command "podman volume mount" in rootless mode,
must execute `podman unshare` first
$ sudo podman volume mount myvol       # rootful
/var/lib/containers/storage/volumes/myvol/_data
$ sudo podman volume unmount myvol
myvol
```

Tracing why: `~/git/podman/libpod/volume_internal.go`'s own
`needsMount()` returns `false` for the "local" driver with no
filesystem-type/device options — this project's own volumes' *only*
real case, always a plain host directory, never backed by any
mount-requiring driver. For that case, even real *rootFUL* `podman
volume mount`'s own `Mount()`/`Unmount()` are genuine no-ops that just
return/ignore the already-existing `_data` path — the same path
[`VolumeStore::data_dir`] already computes, and
[`VolumeInspectView`]'s own `mountpoint` field already reports. The
rootless refusal itself is a real user-namespace re-exec requirement
(`registry.UnshareNSRequired`/`ParentNSRequired`) that exists purely
for the *other* real cases (a pluggable/`image`-driver volume that
genuinely does need a privileged mount syscall) — it has no bearing
at all on the one case this project actually has.

Faithfully reproducing that rootless refusal here would only make
this command strictly less useful (there is no `ociman unshare` to
retry with, nor any genuine privilege boundary being crossed to
justify inventing one) without matching any real constraint of this
project's own design — the same reasoning `0355`/`0357` already used
to deliberately keep a faster/simpler behavior over a real upstream
one that doesn't actually apply here. `ociman volume mount`/`unmount`
therefore always succeed, matching real *rootful* `podman`'s own
output exactly.

## Implementation

New `VolumeCommand::Mount { name }`/`Unmount { name }`. `cmd_volume_
mount` checks `store.exists(name)`, then prints
`store.data_dir(name).display()` — the exact same path
`VolumeInspectView::mountpoint` already computes, no new logic.
`cmd_volume_unmount` checks existence and prints the volume's own
name, matching real podman's own checked-directly output; genuinely
does nothing else at all (there is nothing to undo).

## Verified

New tests in `tests/tests/ociman_volume.rs`:
`volume_mount_prints_the_real_data_directory_path` (asserts the
printed path matches `volume inspect`'s own `mountpoint` exactly, and
is a real, already-existing directory);
`volume_unmount_is_a_real_no_op_that_prints_the_name` (asserts the
volume's own directory survives fully intact afterward);
`volume_mount_and_unmount_of_an_unknown_volume_are_clear_errors`
(matching `volume export`/`import`'s own identical, already-
established convention for the same case). All 27 pre-existing
`ociman_volume.rs` tests re-run unmodified and still pass.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures, full clean
run, no flakes), `python3 ci/guards.py`, `cargo deny check`, `bash
ci/native-ci.sh`, `bash ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).
