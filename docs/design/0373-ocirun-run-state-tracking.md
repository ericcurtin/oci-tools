# Design note 0373: `ocirun run` gets a real, tracked state record

Status: implemented
Scope: `bin/ocirun/src/main.rs`, `tests/tests/ocirun_run.rs`,
`tests/tests/ocirun_hooks.rs`, `README.md`.

## What this closes

Found while scoping `ocirun run -d`/`--detach` (a candidate from the
last research pass): `cmd_run` never opened a `StateStore` at all —
confirmed by direct grep, `root` (the `--root` global flag every
other subcommand already threads through) was never even a parameter
of `cmd_run`'s own signature. A concurrent `ocirun state`/`list`/
`exec`/`kill` against the same id, issued from an entirely separate
invocation while the original `ocirun run` was still blocked in the
foreground, saw *nothing at all* — the exact same real gap `ociman
run`'s own `record_running` (`0023`) already closed for `ociman`
itself, never actually closed for the lower-level `ocirun` this whole
time.

## Real, checked-directly confirmation this is a genuine divergence

Read `~/git/runc/utils_linux.go`'s own `startContainer` directly,
expecting `run`/`create` to differ architecturally — found instead
that both call the exact same `createContainer` (the real,
state-persisting factory call) unconditionally, regardless of
`action`. Confirmed empirically too: a real installed `runc run` (no
`-d`, no `--keep`) leaves nothing behind in `runc list`/`state`
*after* it exits — but that's specifically because `r.run()`'s own
`shouldDestroy` cleanup (`!cmd.Bool("keep")`) runs as a `defer`
*after* the foreground wait completes, not because the container was
never tracked in the first place. During the blocking window itself,
the state genuinely exists and is genuinely queryable — this
project's own `ocirun run` had no equivalent of that window at all.

## Implementation

`cmd_run` now takes `root: &Path` (the same, already-resolved global
flag `create`/`start`/`list`/`state`/`delete`/`kill` all already use).
Before launching, `store.create(id, dir, &rootfs, annotations)` — the
identical call `cmd_create` already makes. The pid-reporting callback
already threaded through `run_reporting_pid` for `--pid-file` now also
writes `Status::Running` + the real pid, the same real moment
`ociman`'s own `record_running` writes at. Once `run_reporting_pid`
returns (success or failure), the state is unconditionally removed —
matching real runc's own checked-directly default exactly: a plain,
foreground `ocirun run` leaves nothing behind for a later `ocirun
state`/`list`/`delete` to ever need to see, whether the container
actually ran (any exit code) or the launch itself failed partway
through. Real runc's own `--keep` (skip that removal) isn't
implemented here yet — a real, honest, deliberately narrower first
slice; `-d`/`--detach` (a genuinely bigger feature, needing a real
forked "keeper" process the same way `ociman run -d`'s own `0098`
built one) is also deliberately deferred.

Two pre-existing test files' own local `ocirun_run` helpers
(`ocirun_run.rs`, `ocirun_hooks.rs` — the only two, confirmed by grep)
never passed `--root` at all before this, since there was nothing to
pass it *to*. Both updated to pass an isolated, per-test `--root`
(computed as a sibling directory of the bundle itself, so none of
their existing 24 call sites needed to change at all) — matching
every other test file's own already-established "always an isolated
root, never this project's own real shared default" convention,
rather than relying on the real `$XDG_RUNTIME_DIR/ocirun` default
(which, empirically, DID still work correctly with no residue left
behind — confirmed directly before making this change — but is a
needless, inconsistent risk on a shared host).

## Verified

New tests in `tests/tests/ocirun_run.rs`:
`run_is_visible_to_a_concurrent_state_query_then_fully_removed_after_
exit` (a real, separately-spawned `ocirun run` child process; a
concurrent `ocirun state`/`list` from this test's own, entirely
separate process confirms it sees the real, running container; killed
early, then confirms the state is completely gone once the `ocirun
run` process itself has fully exited);
`run_of_an_id_already_in_use_is_a_clear_error` (a second `ocirun run`
reusing an id still genuinely in use by a first, still-running one is
a real, clear error, and the first container is left completely
unaffected). Both needed a small, dedicated polling helper
(`wait_for_status_tolerating_not_yet_created`) distinct from the
shared `oci_tools_tests::wait_for_status`: every one of that shared
helper's own existing call sites polls only *after* an `ocirun
create`/`start` invocation has already returned successfully
(guaranteeing a state record already exists), while these new tests
start polling the instant a freshly `spawn()`ed child process is
launched, genuinely racing against whether `store.create` has run
yet at all. All 26 pre-existing tests across both updated files
re-run unmodified and still pass.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures, full clean
run, no flakes), `python3 ci/guards.py`, `cargo deny check`, `bash
ci/native-ci.sh`, `bash ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip). This change touches `ocirun run`, a directly
`ci/bench.sh`-measured hot path — re-run specifically: 3.2ms mean,
unchanged from the `0372` baseline (3.1ms) within noise — the extra
`StateStore::create`/`write`/`remove` I/O (already on a `tmpfs`-backed
root, per this flag's own documented recommendation) costs nothing
measurable.
