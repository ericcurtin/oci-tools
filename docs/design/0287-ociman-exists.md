# Design note 0287: `ociman container/image/volume exists`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_exists.rs`.

## The one basic CRUD verb this project never had, for any resource

Every other basic verb — create, list, inspect, remove — already
existed for containers, images, and volumes here. `exists` (a silent,
exit-code-only existence check real shell scripts and CI pipelines
lean on constantly, e.g. `podman container exists foo && podman rm
foo`) was the one conspicuous exception, missing entirely for all
three resources. Checked directly against a real installed `podman
container exists --help`/`image exists --help`/`volume exists --help`:
all three print nothing at all and exit `0` (found) or `1` (not
found); real docker has no equivalent of any of the three.

## Why a new `container`/`image` subcommand family, when every other verb here is flat

This project deliberately keeps every other container/image verb flat
and top-level (`ociman ps`, `ociman rm`, `ociman rmi`, `ociman
inspect`, ...) rather than mirroring real podman's `podman container
ps`/`podman container rm`/... nested aliases. `exists` is a genuine
exception to that choice, not an inconsistency: checked directly,
neither real docker nor real podman documents a bare top-level
`podman exists`/`docker exists` at all — `container exists`/`image
exists`/`volume exists` are the *only* way either real tool exposes
this verb. Since `ociman volume` already has an established
`VolumeCommand` subcommand family (matching real podman's own
`podman volume` family exactly), adding `Exists` to it was
straightforward; `container`/`image` needed a new, minimal
single-purpose family each (`ContainerCommand`/`ImageCommand`), whose
only real reason to exist right now *is* to host this one command —
not a step toward flattening every other verb, which stays exactly as
it is.

## `--external` is a real, accepted no-op

Real `podman container exists --external` also checks non-Podman-
managed storage containers. This project has no such concept at all
(every container `ociman` ever creates is already fully tracked), so
the flag is accepted for CLI compatibility but never changes the
result.

## Resolution reuses existing, already-tested lookups

- `container exists`: a new `container_exists` helper mirrors
  `resolve_container_id`'s own two-step id-then-name lookup, but
  reports `false` for "not found" instead of a hard error (unlike
  every other command here, a missing container is never an error for
  `exists`).
- `image exists`: reuses `resolve_by_reference_or_id` directly (the
  exact same tag-then-real/short-ID resolution `ociman inspect`/`rmi`/
  `tag` already share).
- `volume exists`: reuses the volume store's own existing `get`
  lookup (the same one `ociman volume inspect` already uses).

No new resolution logic, no new storage, anywhere.

## Verified

Integration (`tests/tests/ociman_exists.rs`, four new tests):

- `volume exists`: exit `1`/silent for a missing volume, exit `0`/
  silent once created.
- `container exists`: exit `1`/silent for a missing container; exit
  `0` for a real, stopped container found both by its own `--name` and
  by its generated id.
- `container exists --external`: accepted, still a real no-op.
- `image exists`: exit `1`/silent for a missing image; exit `0` for a
  real seeded image found both by tag reference and by a real short
  image ID.

Every assertion also directly checks `stdout`/`stderr` are both
completely empty on both outcomes, matching the real, checked-directly
"no output at all, either way" behavior of all three real commands.

Full workspace: `cargo build`/`test --workspace` (111 test binaries),
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets --
-D warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh`.

## Still ahead

Real podman also has `pod exists`/`network exists`, both out of scope
(this project has no pod or managed-network concept at all yet).
</content>
