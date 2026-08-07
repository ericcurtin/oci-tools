# Design note 0566: `ocirun delete --force` on a nonexistent container

Status: implemented
Scope: `bin/ocirun/src/main.rs`, `tests/tests/ocirun_lifecycle.rs`.

## What this closes

A real, previously-existing exit-code/behavior mismatch: `ocirun
delete --force <id>` on a container that doesn't exist at all was a
hard error before this change, where both real reference runtimes
(`runc`/`crun`) genuinely succeed. `docs/design/0017` (the note that
originally implemented `create`/`start`/`kill`/`delete`) only ever
documents `delete --force` for a still-*running* container, never
this "missing container entirely" case — nothing between `0017` and
`0565` ever revisits it.

## Real, checked-directly confirmation — live-verified against real
installed binaries, not just source

- `~/git/runc/delete.go:47-64`: `getContainer` fails with
  `errors.Is(err, libcontainer.ErrNotExist)` → unconditional
  best-effort `os.RemoveAll(path)` (its own error only logged to
  stderr, never fatal) → `if force { return nil }`, else falls
  through to `return err`.
- `~/git/crun/src/libcrun/container.c:1833-1852`
  (`container_delete_internal`): `libcrun_read_container_status`
  fails; `if (force && crun_error_get_errno(err) == ENOENT)` releases
  the error, attempts a best-effort `libcrun_container_delete_status`
  (its own error discarded), `return 0`.
- Live-verified directly against real installed `runc 1.x`/`crun`
  binaries on this host (not just reading source):
  ```
  $ sudo runc --root /tmp/x delete --force totally-nonexistent-id; echo $?
  0
  $ sudo runc --root /tmp/x delete totally-nonexistent-id; echo $?
  level=error msg="container does not exist"
  1
  $ sudo crun --root /tmp/x delete --force totally-nonexistent-id-2; echo $?
  0
  $ sudo crun --root /tmp/x delete totally-nonexistent-id-2; echo $?
  cannot open directory ...: No such file or directory
  1
  ```
  Both reference runtimes agree on the identical rule: `--force`
  absorbs a "container doesn't exist at all" error into success;
  without it, still a hard error either way.

## Real functional gap, not a faithful no-op

Before this change: `ocirun --root /tmp/y delete --force
totally-nonexistent-id` exited `1` with `error: container
"totally-nonexistent-id" does not exist` — a real, observable
divergence a supervisor's own cleanup-on-teardown script (relying on
real runc/crun's own tolerant exit code, sometimes without even a
`|| true`) would trip over.

## Why this is narrow and safe

Touches only `cmd_delete`'s own early error-handling branch — no new
struct fields, no new persisted state, no threading anything through
`create`/`start`/`kill`/`exec`/`update`/`pause`/`resume` (all
completely unaffected). The success path (container genuinely exists)
is byte-identical to before: the `Ok(state) => state` arm performs
the exact same `store.load(id)?`-equivalent work the old code did.
Only the exceptional `NotFound` branch gained new logic — reusing the
already-existing `StateStore::remove`/`StateError::NotFound` plumbing
rather than adding anything new.

## Implementation

`cmd_delete` now matches on `store.load(id)`'s own result explicitly
instead of a plain `?`: on `Err(StateError::NotFound(_))`, a
best-effort `store.remove(id)` is always attempted first (in case
`state.json` alone is missing but a stray directory still exists —
matching both reference runtimes' own identical "always attempt
cleanup" behavior), then either `Ok(())` when `force` is set, or the
original, unmodified `NotFound` error re-surfaced otherwise (preserving
today's exact error text/behavior for the non-`--force` case). Every
other `StateError` variant (`Corrupt`, `InvalidId`, `Io`,
`AlreadyExists`) still propagates unchanged via `Err(e) => Err(e.into())` —
this fix only ever touches the specific "doesn't exist at all" case,
never masking a genuinely different failure.

## Tests

Two new integration tests in `tests/tests/ocirun_lifecycle.rs`:
`delete_force_on_a_nonexistent_container_is_a_silent_success` and
`delete_without_force_on_a_nonexistent_container_is_still_a_clear_
error` — proving the new behavior and confirming the fix is strictly
additive (still a real, immediate error without `--force`).

Manually verified end to end beyond the automated tests, including a
direct side-by-side comparison against real installed `runc`/`crun`:
a still-*running* container without `--force` is still correctly
refused; `--force` on that same still-running container still
correctly kills and removes it (both pre-existing behaviors,
completely unaffected by this change) — confirmed by hand with a real
bundle and a genuinely running process.

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (129
test-result blocks, all passing — no new test file added, so the
block count is unchanged from `0565`; `RUST_TEST_THREADS=2` given
this host's own heavy, persistent concurrent-session CPU contention
this same day), `python3 ci/guards.py` (clean), `cargo deny check`
(clean), `bash ci/native-ci.sh` (one isolated `ociman_logs.rs` flake
under the same contention, confirmed transient by an immediate
isolated rerun, then a fully clean run on the second attempt), `bash
ci/build-deb.sh` (clean on the first attempt, real `dpkg -i`/
`--version`/`dpkg -r` round trip). `ocirun delete` itself is not
directly benchmarked by `ci/bench.sh` (only used for cleanup between
timed runs, with `|| true`, which this fix now makes genuinely
unnecessary rather than papering over the bug) — no benchmark rerun
needed; the success path (the one actually exercised by every
benchmarked scenario) is completely unchanged.

## Deliberately still out of scope

`ociman delete`/`ociman rm` and `ocicri`'s own container-removal RPCs
each have their own separate, already-established "unknown container"
handling (checked at the time each of those was implemented) — this
note only closes `ocirun delete`'s own gap, the low-level runtime
layer real `runc`/`crun` themselves define this exact rule for.
