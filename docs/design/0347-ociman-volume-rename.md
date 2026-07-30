# Design note 0347: `ociman volume rename`

Status: implemented
Scope: `bin/ociman/src/volume.rs`, `bin/ociman/src/main.rs`,
`tests/tests/ociman_volume.rs`.

## What this closes

`ociman`'s own `VolumeCommand` enum had `create`/`ls`/`inspect`/`rm`/
`prune`/`exists`/`export`/`import` but no `rename` at all — a genuine
gap versus real `podman volume rename`, surveyed and flagged after
`0346`.

## Real, checked-directly semantics

Read `~/git/podman/libpod/runtime_volume.go`'s own `RenameVolume`
directly:

- Renaming a volume to its own current name is a real, silent no-op
  success (`if vol.Name() == newName { return vol, nil }`) — checked
  *before* the new name's own validation/collision checks.
- Only local-driver volumes can be renamed (this project has exactly
  one driver — its own plain local directory — so this restriction is
  always trivially satisfied, not implemented as an explicit check).
- Refuses a volume currently in use by any container (`r.state.
  VolumeInUse`) — the *same* rule `volume rm` already enforces with no
  `--force` given — but, checked directly, **`rename` has no
  `--force`-equivalent escape hatch at all**: no parameter on
  `RenameVolume` corresponds to one, unlike `rm`'s own real `--force`.
  This project's own `containers_using_volume` check (already shared
  by `rm`/`prune`) is the exact functional equivalent of both real
  podman's `VolumeInUse` *and* its separate live `MountCount` gate
  (this project's volumes have no independent "currently mounted"
  bookkeeping beyond "does some container's own bundle spec reference
  this path" — the same simplification `rm`'s own doc comment already
  documents).
- A new name that already resolves to a real, different volume is a
  clear error (`~/git/podman/libpod/sqlite_state.go`'s own
  `RenameVolume`: a SQLite `UNIQUE` constraint violation on the rename
  `UPDATE`, mapped to `ErrVolumeExists`).
- Real podman's own storage layer genuinely moves the volume's own
  on-disk directory too (`os.Rename(oldPath, newPath)`), not just a
  database record — this project's own equivalent primitive doesn't
  have a separate database-vs-storage-path split to keep in sync at
  all, so a single `fs::rename` of the whole `<name>/` directory
  (`_data` and `metadata.json` together) does the same real, atomic
  move in one step.
- `podman volume rename` prints **nothing** on success (confirmed
  directly: its own `RunE` never calls anything print-like) — ported
  exactly, unlike `volume rm`, which does print the removed name.

## Implementation

`VolumeStore::rename` (`volume.rs`) is a low-level primitive only:
`fs::rename` the whole directory, then rewrite `metadata.json` so its
own `name` field matches the new directory (`list()`/`get()` both
trust that field verbatim rather than inferring it from the directory
name — leaving it stale would have been a real, latent bug). Existence
of the old name, non-existence of the new one, and the in-use check
are all the *caller's* responsibility, matching this module's own
established "low-level primitive, business rules live in `main.rs`"
split (`remove`'s own identical convention, via `cmd_volume_rm`).

`cmd_volume_rename` (`main.rs`) orders its checks exactly like real
podman: same-name early return first, then `is_valid_volume_name`
(the existing validator `create` already uses), then the already-
exists check, then `containers_using_volume` (reused verbatim from
`rm`/`prune` — a genuine zero-new-logic reuse of an already-shared
helper), then the actual `VolumeStore::rename` call.

## Verified

New unit tests in `volume.rs`:
`rename_moves_the_real_directory_and_updates_the_persisted_name`
(confirms real file content survives the move, and the persisted
`name` field is updated, not left stale),
`rename_of_a_nonexistent_volume_is_a_real_io_error`.

New integration tests in `ociman_volume.rs`:
`volume_rename_moves_real_content_to_the_new_name` (end to end through
the real CLI, including the "prints nothing on success" check),
`volume_rename_to_its_own_current_name_is_a_silent_no_op`,
`volume_rename_to_an_already_existing_different_volume_is_a_clear_error`
(also confirms neither volume is touched),
`volume_rename_of_an_unknown_volume_is_a_clear_error`,
`volume_rename_refuses_a_volume_a_running_container_depends_on` (a
real running container, matching `volume_rm`'s own identical existing
test shape).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (113 test-result blocks,
0 failures), `python3 ci/guards.py`, `cargo deny check`.

## Still ahead

`ociman volume ls -q`/`--quiet` remains a separate, similarly-small,
not-yet-scoped candidate surveyed alongside this one.
