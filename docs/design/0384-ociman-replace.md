# Design note 0384: `ociman run`/`ociman create --replace`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_name.rs`,
`docs/design/0032-ociman-name.md`.

## What this closes

`docs/design/0032`'s own original "what's still not here" section
(written when `--name` was first added) explicitly listed `--replace`
as not implemented — a name conflict was always a hard error, with no
way to say "remove the old one and use this name anyway." Still true
today, confirmed by grep: no `--replace` flag existed anywhere in
`RunArgs`/`Command::Run`/`Command::Create`.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/create.go`'s own
  `replaceContainer` (lines 208-217): requires `--name` to be set
  (`"cannot replace container without --name being set"` otherwise —
  matched here verbatim), then force-stops-and-removes (`Force: true,
  Ignore: true`) any existing container of that name.
- Confirmed real podman's own flag definition
  (`cmd/podman/common/create.go`'s `DefineCreateFlags`): `--replace` is
  a *single* shared flag definition applying identically to both real
  `podman run` and `podman create` (both call the same
  `replaceContainer` function from the same file) — exactly matching
  the shape of `ociman`'s own shared `RunArgs`/`prepare_container`
  design, so this needed no separate `Command::Run`/`Command::Create`
  variant changes at all, just one new field on `RunArgs`.
- Confirmed the replace step runs strictly *before* the real create
  path (`create()`, lines 171-184) — matched here by inserting it
  before the existing name-uniqueness check in `prepare_container`.
- `remove_container`'s existing force path already does exactly what
  real podman's `Force: true` does (stop, with this project's own
  already-established faster immediate-`SIGKILL`-no-grace-period
  default, then remove storage) — fully reusable as-is.

## A real fork-safety bug found and fixed along the way

Manually testing `ociman run -d --replace --name X ...` against a
still-*running* prior container of that name **panicked**:
`fork() called with 3 threads alive in this process (expected exactly
1)`. Root cause: the exact same class of bug design note **0159**
already found and fixed for `ociman restart` — `remove_container`'s
own force-kill path calls `reset_failed_systemd_scope`, which spawns a
background D-Bus thread (`oci_runtime_core::systemd_cgroup::
reset_failed_unit`, deliberately never joined). `--replace` uniquely
calls `remove_container` and then, in the very same process, launches
a brand new container — which forks (the detached keeper, or the
foreground container process itself). If that background thread was
still alive at the moment of that later `fork()`, the calling process
was not actually single-threaded, violating `process::fork`'s own
documented safety contract.

Fixed the same way 0159 fixed it for `cmd_restart`: `remove_container`
gains a `reset_scope: bool` parameter (mirroring `stop_container`'s
own identical parameter exactly) — `false` for the `--replace` call
site inside `prepare_container`, which instead captures the replaced
container's own old state *before* removing it, threads it out through
a new `PreparedContainer::replaced: Option<(String, PersistedState)>`
field, and lets `cmd_run`/`cmd_create` perform the actual
`reset_failed_systemd_scope` call themselves — but only *after* their
own next fork has already happened (right after `launch_detached_and_
confirm`/`run_and_finalize` return for `cmd_run`; immediately for
`cmd_create`, which — confirmed directly — never forks any real
process of its own at all, so no deferral is even needed there, only
the same handoff plumbing for structural consistency). All 5 other
pre-existing `remove_container` call sites (the `rmi --force` cascade,
`system reset`, `container prune`, both `cmd_rm` targets) pass
`reset_scope: true` unchanged — none of them are ever followed by a
later fork in the same process, so the original, immediate-reset
behavior stays correct and unchanged for every one of them.

## Implementation

- `RunArgs` gains `replace: bool` (`#[arg(long)]`), right after
  `name` — automatically available on both `ociman run --replace` and
  `ociman create --replace` via the existing shared-flatten mechanism,
  no other struct/dispatch changes needed.
- `prepare_container` gains an eager guard (`--replace` without
  `--name` is a real, immediate error, matching real podman's exact
  wording) and the name-collision `match` now has three arms: `Ok(_)
  if args.replace` (capture old state, force-remove with `reset_scope:
  false`, record the deferred handoff), `Ok(_)` (unchanged existing
  hard error), `Err(_)` (unchanged existing pass-through).
- `cmd_run`/`cmd_create` destructure the new `PreparedContainer::
  replaced` field and perform the deferred `reset_failed_systemd_
  scope` call at the correct, fork-safe point each.

## Tests

Four new tests in `tests/tests/ociman_name.rs`:
`run_replace_removes_an_existing_stopped_container_of_the_same_name`
(the ordinary case — new id, count stays 1),
`run_replace_removes_an_existing_running_container_of_the_same_name`
(the test that originally hit the real fork-safety panic above — a
genuinely running prior container, replaced via `run -d --replace`,
confirming no panic, a new running container, and exactly one
container remaining afterward),
`run_replace_without_name_is_a_clear_error` (exact error message
match), and `create_replace_removes_an_existing_container_of_the_same_
name`. All 9 tests in `ociman_name.rs` (5 pre-existing + 4 new) pass.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures), `python3
ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh`, `bash
ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg -r` round trip).
This change touches `prepare_container` (a directly `ci/bench.sh`-
measured hot path when `--replace` is given at all — a no-op branch
otherwise) — targeted `hyperfine` re-run: `ociman run --rm` 32.0ms,
matching the recorded baseline (32.7ms) within noise, no regression.
