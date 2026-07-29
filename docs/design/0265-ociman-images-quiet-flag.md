# Design note 0265: `ociman images -q`/`--quiet`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_images.rs`.

## A real self-inconsistency in `ociman`'s own CLI

Not a podman-parity gap first — a real inconsistency *within
`ociman`'s own CLI* found while comparing it against itself: `ociman
ps` already has `-q`/`--quiet` (list just the IDs), but `ociman
images` — an equally simple list command — didn't. Real `docker
images -q`/`podman images -q` both have exactly this mode too, so
closing the internal inconsistency also closes a real podman-parity
one at the same time.

## Mechanical, low-risk

`Command::Images` (a unit variant) gained one new field
(`quiet: bool`, `-q`/`--quiet`), threaded into `cmd_images` the same
way `cmd_ps`'s own `quiet: bool` already works: a new, early-return
branch (checked before the existing `--json`/plain-table branches, so
it wins over both) that prints one line per image — the exact same
12-hex-char digest prefix the plain table's own `DIGEST` column
already computed, now factored into one shared closure
(`short_digest`) so the two can never silently compute two different
truncations of the same value. No new lower-level code, no change to
any existing behavior when the flag is absent.

## Verified

Integration (`tests/tests/ociman_images.rs`):

- An empty store prints nothing at all in quiet mode.
- Both `-q` and `--quiet` print the identical 12-hex-char digest the
  plain table's own `DIGEST` column shows for the same image.
- Two tags of the same real image each get their own line (matching
  real `podman images -q`'s own identical one-row-per-tag behavior,
  the project's own already-established convention, unrelated to this
  new flag) — both lines share the same real digest.

Full workspace: `cargo build`/`test --workspace` (110 test binaries),
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets --
-D warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh`.

## Still ahead

`ociman images --filter` (`dangling=`/`label=`/`before=`/`since=`/
`reference=`) and `ociman rm --all` remain real, similarly-scoped gaps
for a future increment.
