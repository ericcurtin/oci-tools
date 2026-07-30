# Design note 0374: `ocirun run --keep`

Status: implemented
Scope: `bin/ocirun/src/main.rs`, `tests/tests/ocirun_run.rs`,
`README.md`.

## What this closes

The natural, immediate follow-up `0373` explicitly deferred: real
runc's/crun's own `run --keep` flag — skip the post-exit state
removal a plain `ocirun run` now always performs, leaving the
container's own state queryable (and deletable) afterward instead.

## Real, checked-directly confirmation

`~/git/runc/utils_linux.go`: `run`'s own `shouldDestroy` is exactly
`!cmd.Bool("keep")` — nothing else about `run` changes when `--keep`
is given, just that one boolean gating the same `runner.destroy()`
call `0373` already found. `~/git/crun/src/libcrun/container.c`:
`LIBCRUN_RUN_OPTIONS_KEEP` gates the identical
`force_delete_container_status` call the same way. Both real
implementations agree: `--keep` is purely "don't clean up afterward",
nothing more.

## Implementation

`Command::Run` gains a new `keep: bool` field
(`#[arg(long)] keep: bool`). `cmd_run` takes it as a new parameter and
the `0373`-added unconditional `let _ = store.remove(id);` becomes
`if !keep { let _ = store.remove(id); }`.

No separate "write `Stopped`" step is needed for the `--keep` case:
`state` was last written `Status::Running` with the container's own
real pid inside the pid-reporting callback, and
`PersistedState::effective_status` (already relied on everywhere else
in this project) re-derives `Stopped` lazily from that pid no longer
being alive the next time anything queries it — the same "process
death is the only signal that matters" convention this whole state
store already established for every other command, not a new one
invented here just for `--keep`.

## Tests

Two new tests in `tests/tests/ocirun_run.rs`:
`run_keep_leaves_a_real_stopped_state_behind_for_a_later_delete` (a
real `ocirun run --keep` exits, `ocirun state` on the same id reports
`stopped`, and a subsequent `ocirun delete` actually removes it — a
`ocirun state` query after that delete correctly fails again) and
`run_without_keep_removes_the_state_entirely` (the same real assertion
`0373`'s own
`run_is_visible_to_a_concurrent_state_query_then_fully_removed_after_exit`
already makes, kept here too as a direct, explicit contrast right next
to the `--keep` test above). All 18 tests in this file (16
pre-existing + 2 new) pass; the 12 pre-existing tests in
`ocirun_hooks.rs` (unaffected, `keep` defaults to `false` and neither
test file's own local `ocirun_run` helper passes it) still pass too.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures, full clean
run), `python3 ci/guards.py`, `cargo deny check`, `bash
ci/native-ci.sh`, `bash ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip). This change touches `ocirun run`, a directly
`ci/bench.sh`-measured hot path — re-run specifically: 3.0ms mean,
unchanged from the `0373` baseline (3.2ms) within noise — the new
`if !keep` branch costs nothing measurable in the (default, non-kept)
path `ci/bench.sh` itself times.
