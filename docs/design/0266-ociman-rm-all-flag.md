# Design note 0266: `ociman rm --all`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_ps.rs`.

## Closing another real, checked-directly gap

`ociman rm` only ever removed exactly one, explicitly named container.
Real `podman rm --all` — confirmed directly against a real installed
`podman rm --help` (`docker rm` has no such flag at all: `docker rm
$(docker ps -aq)` is its own closest equivalent) — removes every
container in one call instead. This project's own `ocibox rm --all`
already established the exact clap shape and policy for this same
"single name or `--all`, mutually exclusive" pattern, making this a
direct, low-risk port of an already-proven pattern to `ociman`, not a
new design.

## Semantics, checked directly

- `--force`'s existing per-container gate (refuse a non-`Stopped`
  container unless given) is completely unchanged — `--all` alone,
  without `--force`, still leaves a running (or created-but-never-
  started) container untouched, confirmed directly against real
  `podman rm --all`'s own identical behavior (its own help text's
  worked example is literally `podman rm --force --all`, not `--all`
  alone).
- Every container is still attempted even if one fails partway
  through — matching both real `podman rm`'s own multi-target
  behavior and this project's own `ocibox rm --all`'s identical
  policy — with the first real error still surfaced (so a genuine
  failure is never silently swallowed) once every container has had
  its own attempt.
- A real, silent no-op on an already-empty store, matching this
  project's own established "empty is a valid, unremarkable state"
  convention (`ocibox rm --all`'s own identical rule).
- `--all` and an explicit ID together is a clear error, never an
  ambiguous silent choice between the two.

## Verified

Integration (`tests/tests/ociman_ps.rs`, four new tests):

- `--all` removes every real, stopped container, printing each
  removed id (one line per container); a second `--all` call on the
  now-empty store is a real, silent no-op.
- `--all` combined with an explicit ID is a clear, immediate error.
- A mix of one real stopped container and one non-stopped
  (created-but-never-started) record: `--all` without `--force`
  removes the stopped one, leaves the non-stopped one untouched, and
  still surfaces the one real failure; `--all --force` together then
  removes everything.

Full workspace: `cargo build`/`test --workspace` (110 test binaries),
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets --
-D warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh`.

## Still ahead

`ociman rm <id1> <id2> ...` (multiple explicit IDs in one call, real
podman's own additional supported shape beyond just one name or
`--all`) is implemented in `0267`. `ociman images --filter` remains a
real, similarly-scoped next candidate.
