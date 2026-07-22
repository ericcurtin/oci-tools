# Design note 0173: `ociman volume`

Status: implemented
Scope: `bin/ociman/src/volume.rs` (new module: `VolumeStore`,
`VolumeRecord`, `is_valid_volume_name`); `bin/ociman/src/main.rs`
(`Command::Volume`, `VolumeCommand`, `cmd_volume_{create,ls,inspect,
rm,prune}`, `open_volume_store`; `VolumeHost`/`VolumeSpec` split out of
the previous `ParsedVolume`, `resolve_volume_host`, `containers_using_
volume`); `tests/tests/ociman_volume.rs`.

## Closing a real, explicitly-named deferred gap

`parse_volume`'s own doc comment used to say, in as many words: "named/
anonymous volumes are not supported yet... rejected with a clear error
rather than silently misinterpreted as something else." This
increment closes the *named*-volume half of that gap (anonymous —
container-path-only — volumes remain out of scope, see below).

## Real podman's own on-disk layout, matched exactly

Checked directly against a real installed `podman volume inspect`'s
own `Mountpoint`: `<storage-root>/volumes/<name>/_data`. `VolumeStore`
matches this exactly, plus a small `metadata.json` per volume (just
`name`/`created_at` — deliberately narrower than real podman's own
much larger volume record: no `Labels`/`Options`/`MountCount`/
`NeedsCopyUp`/`NeedsChown`/`LockNumber`, all real driver-level
bookkeeping this project's own single, fixed "local directory" driver
has no equivalent need for).

## Real podman's own volume-name rule, not real moby's

Checked directly against a real `podman volume create`'s own error
text (`names must match [a-zA-Z0-9][a-zA-Z0-9_.-]*`) and confirmed by
testing: a real `podman volume create x` (a single character)
succeeds. Real moby's own `RestrictedNamePattern` looks almost
identical but requires a *second* character (`+`, not `*`) — this
project matches podman's own more permissive rule instead, since
podman is `ociman`'s own primary reference implementation throughout.

## `-v NAME:/path[:ro]`: a real, minimal-blast-radius refactor

`parse_volume` (pure, no filesystem/store access, matching this
project's own "parsing is pure" convention elsewhere) now returns a
`VolumeSpec` whose own host side is a `VolumeHost` enum
(`Path(String)` for an already-absolute bind-mount source,
`Named(String)` for a volume name that passed `is_valid_volume_name`)
rather than the old, always-a-bare-`String` `ParsedVolume`.
`resolve_volume_host` (a new, small, separate function — the one place
side effects happen: creating a missing bind-mount directory, or
auto-creating a named volume on first reference) turns either variant
into a real, already-existing host directory string, producing the
*same* `ParsedVolume` shape `synthesize_spec` already expected — so
`synthesize_spec` itself needed **zero** changes at all; the entire
new capability is additive, layered strictly *before* the point where
a resolved volume list already existed.

Auto-creation on first use matches real `docker run -v name:/path`/
`podman run -v name:/path` exactly (confirmed directly: the volume
shows up in `ociman volume ls`/`podman volume ls` immediately after,
with no separate `volume create` ever needed first).

## Removal safety: reusing what's already on disk, not a separate
parallel record

`ociman volume rm`/`ociman volume prune` both need to know "is this
volume currently referenced by any container" — rather than inventing
a new, separate bookkeeping record that could silently drift out of
sync with reality, `containers_using_volume` checks every container's
own **already-persisted** `config.json` mounts directly for a `source`
matching the volume's own real `_data` directory (the exact same real
path `resolve_volume_host` itself would have written there at that
container's own creation time). `--force` on `rm` only ever detaches
the volume — the dependent container(s) are left completely untouched
(matching real `podman volume rm --force`'s own identical behavior,
deliberately different from `ociman rmi --force`'s own "also remove
the dependent containers" convention for images, since removing a
*volume* was never going to delete a container either way in real
podman).

## A small, deliberate deviation from real podman: `volume ls` with
zero volumes

Real `podman volume ls` prints *nothing at all* (not even the table
header) when there are no volumes — confirmed directly. This project's
own already-established convention for every other list command
(`ociman images`'s own `"no images"`, `ociman ps`'s own `"no
containers"`) is a friendly empty-state message instead; matched here
too (`"no volumes"`), for this project's own internal consistency,
rather than copying podman's own slightly different empty-table
behavior for this one specific subcommand.

## Verified against real `docker volume`/`podman volume`

Real `podman volume create`/`inspect`/`ls`/`rm` were each run directly
during development to confirm the exact CLI shapes and on-disk layout
this increment matches (see above); `podman run -v name:/path`'s own
auto-create-on-first-use behavior was independently confirmed the same
way before implementing the identical behavior here.

## Tests

`bin/ociman/src/volume.rs` gained 10 unit tests for `VolumeStore`/
`is_valid_volume_name` directly (idempotent create, real `_data`
directory creation, sorted listing, removal, name-validation edge
cases). `bin/ociman/src/main.rs` gained new/updated unit tests for
`parse_volume`'s own named-volume recognition (a name is now a real,
valid `VolumeHost::Named`, not an error; a relative-looking string
that isn't a valid name is still a clear error). `tests/tests/
ociman_volume.rs` adds 12 integration tests: every `create`/`ls`/
`inspect`/`rm` CLI shape (including idempotent create, random-name
create, invalid-name rejection, unknown-volume errors), and — the
real, convincing checks — a genuine `-v name:/path` round trip (write
in one container, auto-create confirmed via `volume inspect`, read
back in a completely separate container), a read-only named volume
rejecting a write, `volume rm` refusing (then, with `--force`,
succeeding at) removing a volume a real still-running container
depends on without touching that container, and `volume prune`
removing only genuinely unreferenced volumes. Full `cargo build
--workspace --locked`/`cargo test --workspace --locked` (2 clean
runs)/`cargo fmt --all --check`/`cargo clippy --workspace --all-
targets --locked -- -D warnings`/`python3 ci/guards.py`/`cargo deny
check`/`bash ci/native-ci.sh` all clean.

## What this doesn't do yet

* Anonymous (container-path-only, no name at all) volumes — a real,
  separate feature this project's own `VolumeStore` has no natural
  place to record under without inventing a name for it anyway; still
  a clear, named error.
* `--label`/`--opt` on `volume create`, `volume export`/`import`,
  `--driver` (any driver besides the one real "local directory"
  behavior this project has at all) — real podman's own further
  surface, all out of scope for this first increment.
* `real podman generate systemd` was considered as an alternative next
  increment but deprioritized: it's explicitly marked `[DEPRECATED]`
  in real podman's own current `--help` output, in favor of Quadlets —
  a real, different mechanism entirely, not a natural next step for
  this project's own CLI-compatibility goals.
