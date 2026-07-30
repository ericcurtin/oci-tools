# Design note 0320: `ociman pause`/`unpause` `--all`/`--cidfile`/multi-target, and a real correctness fix

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_pause.rs`.

## Closing the last remaining single-target-only command family

Following the exact pattern the `kill`/`stop`/`restart`/`rm` streak
(0310-0318) already established, `ociman pause`/`unpause` were still
`id: String` (single-target-only, no `--all`, no `--cidfile`) — real
`podman pause`/`unpause` both accept `CONTAINER [CONTAINER...]`,
`--all`, and `--cidfile` (checked directly, `~/git/podman/cmd/podman/
containers/pause.go`/`unpause.go`; neither has a real `--ignore` at
all, unlike `rm`/`stop`). This note closes it, the last of this
project's own container-lifecycle commands still missing the shape
every sibling command now has.

## A real, pre-existing correctness bug found and fixed along the way

Real `podman pause`/`unpause`'s own state checks (`~/git/podman/
libpod/container_api.go`'s `Pause`/`Unpause`) are strict: pausing an
already-paused container, or unpausing an already-running (not
currently paused) one, are both a real, immediate `ErrCtrStateInvalid`
error — verified live against a real installed podman (exit `125` in
both cases).

This project's own pre-existing `cmd_pause`/`cmd_unpause`, though,
only ever checked `effective_status() == Status::Running` before
acting — and since `Status::Paused` is deliberately never *persisted*
(see its own doc comment; only the display layer computes it from the
live cgroup freezer), `effective_status()` itself can never report
`Paused` at all. A genuinely paused container's `effective_status()`
still reads back `Running`, so the old check passed for *both* a
plain running container and an already-paused one — meaning a
double-`pause` (freezing an already-frozen cgroup) or a double-
`unpause` (thawing an already-thawed one) silently "succeeded"
instead of erroring, confirmed live before this fix. Fixed by
switching to `display_status` (the same real, cgroup-freezer-derived
status `ps`/`inspect` already compute) so a genuinely paused container
is now correctly distinguished from a plain running one.

## Implementation

`Command::Pause`/`Command::Unpause`: `id: String` → `ids: Vec<String>`,
plus new `all: bool` and `cidfile: Vec<PathBuf>` fields on both. A new
shared `cmd_pause_or_unpause(ids, all, cidfiles, freeze: bool)`
function replaces the two near-duplicate `cmd_pause`/`cmd_unpause`
bodies — real podman's own `pause.go`/`unpause.go` are themselves
near-identical (same flags, same `getContainers`-based resolution,
same "attempt every one, report the first real failure at the end"
policy, same "silently skip a container in the wrong state" tolerance
*only* under `--all`), so one function parameterized on `freeze`
matches that symmetry directly rather than duplicating it, per this
project's own "share as much Rust code as possible" pillar. The actual
per-container logic (state check + freeze/thaw) lives in a new,
shared `pause_or_unpause_one`.

`--all`'s own skip condition matches real podman's checked-directly
behavior exactly: `pause --all` only attempts a container whose
`display_status` is genuinely `Running` (silently skipping both an
already-paused one *and* anything not running at all — a never-
started or stopped container); `unpause --all` only attempts one
that's genuinely `Paused` (skipping everything else). Multi-id
resolution follows the same two-phase pattern `kill`/`stop`/`restart`
already established: every given id resolves first (aborting the
whole call on any failure — no `--ignore` exists here to tolerate
one, matching real podman's own identical absence), then every
resolved target is genuinely attempted regardless of an earlier
failure.

## Verified

Manual, end-to-end, cross-checked directly against a real installed
podman (identical container-state mix: one running, one already-
paused, one never-started): `pause --all` pauses only the genuinely
running one, silently leaving both the already-paused and never-
started ones alone — confirmed byte-for-byte matching behavior against
real `podman pause --all` in the same scenario. `pause id1 id2` /
`unpause id1 id2` both work; an unresolvable id among several aborts
the whole call, leaving real containers untouched; `--cidfile` reads
and acts on the named container; `--all` + `--cidfile` is a clear
error; a double-`pause`/double-`unpause` are now real, immediate
errors, matching real podman exactly (previously a silent success).

Integration (`tests/tests/ociman_pause.rs`, 5 new tests, 8 total, 3
pre-existing): double-pause/double-unpause are real errors;
`--all`/`--all` skip containers in the wrong state correctly;
`--cidfile` reads the id and ignores trailing content; `--all` +
`--cidfile` is a clear error for both commands; multiple explicit ids
both pause/unpause, with an unresolvable one among them aborting the
whole call first.

Regression: all 8 `ociman_pause.rs` tests pass; `ociman_stop.rs` (19,
re-verifying `0313`'s own paused-container refusal still works
correctly against the new `display_status`-based check),
`ociman_start.rs` (19), and `ociman_kill.rs` (10, re-verifying `0319`)
all still pass unchanged. Full `cargo test --workspace --locked`: 112
test result blocks, 0 failures.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).

Performance: `ociman pause`/`unpause` are one-shot, offline commands,
not part of any hot-path benchmark tracked in `docs/benchmarks.md`.
The common single-target case is unchanged in cost beyond one extra
real cgroup-freezer file read (`display_status`'s own `is_frozen`
check) — the same real, already-established cost `ps`/`inspect`
already pay for the identical reason. No re-benchmark needed.

## Still ahead

With this note, every container-lifecycle command this project has
(`kill`/`stop`/`restart`/`rm`/`pause`/`unpause`) now supports the full
real podman `--all`/multi-target combination it's actually supposed
to have (`--cidfile` and `--ignore` only where real podman itself has
them). `ociman stop`/`restart` still hard-refuse a genuinely paused
container outright rather than thaw-then-signaling it the way `kill`
now correctly does (`0313`'s own deliberate choice, matching real
podman's own identical refusal) — remains a real, separately-scoped,
deliberately deferred future candidate, since it needs a graceful-
signal-then-escalate policy on top of "the signal takes effect at
all", not just a single delivered signal.
