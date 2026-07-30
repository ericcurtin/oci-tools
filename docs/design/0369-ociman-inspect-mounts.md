# Design note 0369: `ociman inspect`'s own `mounts` field

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_inspect.rs`,
`README.md`.

## What this closes

`ociman inspect` had `-v`/named-volume bind mounts fully implemented
(`0086`, `0173`, `0361`/`0362`) but never surfaced them in its own
output at all — real `podman inspect`'s own `InspectContainerData.
Mounts` (`~/git/podman/libpod/define/container_inspect.go`) is a
top-level, always-present field with exactly this information.

## Why this needed no new tracking at all

Every mount a container ever gets (bind or named-volume) is already
baked verbatim into its own `config.json` at create time
(`synthesize_spec`). `ContainerInspectView::from_state` already has
`state.bundle`'s own path in hand — this is a pure read-and-reshape of
already-persisted data, no new state-store field, no touching
`ocirun`/any shared crate at all.

## Scope: deliberately narrower than real podman's own `InspectMount`

Real podman's `InspectMount` (`Type`/`Name`/`Source`/`Destination`/
`Driver`/`Mode`/`RW`) is itself PascalCase, matching podman's own
richer inspect schema. This project's `ContainerInspectView` has
already established its own simpler, plain-lowercase-field convention
throughout (`id`/`image`/`command`/...), not a field-for-field port of
podman's own JSON shape — the new `ContainerMountView` follows that
same existing convention rather than introducing podman's own casing
for just this one field: `source`/`destination`/`options` (the real,
raw runtime-spec options list verbatim — already carries `"ro"` when
read-only, no separate derived boolean needed) plus `volume:
Option<String>` (the volume's own name, when the mount is one).

## A real, checked-directly filtering gotcha found while building this

Naively surfacing every non-default `spec.mounts` entry showed an
unexpected extra one at first: a container using this project's own
rootless-overlay-rootfs optimization (`rootfs_setup`'s own module doc
comment, `0110`) represents *that* as one ordinary `destination: "/"`
`spec.mounts` entry too (`type: "overlay"`, `lowerdir=`/`upperdir=`/
`workdir=` options) — a real, internal storage-layer implementation
detail specific to this project's own architecture. Real podman/
docker's own overlay2 storage driver is never modeled as a
runtime-spec mounts-array entry at all (applied entirely outside the
OCI runtime spec, before ever invoking runc/crun), so no real tool's
own `inspect --Mounts` output would ever show anything like it either.
`"/"` was added to the existing fixed-default-destinations exclusion
list (`DEFAULT_MOUNT_DESTINATIONS`, mirroring `Spec::example()`'s own
proc/dev/sys/... set) alongside it, found and fixed by actually
inspecting a real overlay-mode container's own output end to end, not
assumed.

## Implementation

New `ContainerInspectView::mounts: Vec<ContainerMountView>`
(`#[serde(skip_serializing_if = "Vec::is_empty")]`, matching `size`'s
own identical opt-in-field convention: a container with nothing extra
reports no `mounts` field at all, not an empty array). New
`extra_mounts(state)`: loads `state.bundle`'s own `config.json`
(best-effort, same "never a spurious failure of the whole `inspect`
command over one more optional display field" philosophy
`display_status`'s own doc comment right above it already
establishes), filters out `DEFAULT_MOUNT_DESTINATIONS`, and maps each
remaining entry — `volume_name_from_mount_source` detects a named
volume purely by path-pattern-matching `source` against
`VolumeStore::data_dir`'s own exact shape (`<root>/volumes/<name>/
_data`), no volume store lookup needed at all.

## Verified

New test in `tests/tests/ociman_inspect.rs`:
`inspect_mounts_reports_bind_mounts_and_named_volumes_but_omits_the_
field_when_empty` — a real bind mount and a real named volume both
correctly surfaced (the volume one carrying its own name, the bind
one not), and a plain container with neither reports no `mounts` key
whatsoever. All 14 pre-existing `ociman_inspect.rs` tests re-run
unmodified and still pass.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures, full clean
run, no flakes), `python3 ci/guards.py`, `cargo deny check`, `bash
ci/native-ci.sh`, `bash ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).
