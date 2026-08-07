# Design note 0570: `ociman stats --all` without `--no-stream`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_stats.rs`.

## What this closes

`docs/design/0560`'s own "Deliberately still out of scope" section
states verbatim: *"real podman's own `--all` also composes with the
default continuous-streaming mode via one unified, periodically-
re-listing channel architecture this project doesn't have yet;
`--all` without `--no-stream` is a clear, honest, immediate 'not yet
supported' error here instead."* This closes that gap: `ociman stats
--all` (and its `ociman container stats --all` alias) now streams
continuously by default, exactly like every other real podman `stats`
invocation, instead of requiring `--no-stream`.

## Real, checked-directly confirmation

- `~/git/podman/pkg/domain/infra/abi/containers.go:1663-1740`
  (`ContainerEngine.ContainerStats`): the real streaming loop —
  `computeStats` (the enumeration + per-container sampling closure)
  is invoked once, its result sent on a channel, then — unless
  `report.Error != nil` or `!options.Stream` — the goroutine sleeps
  `options.Interval` seconds and re-invokes `computeStats` again via
  `goto stream`, forever. Critically, `containerFunc` (`GetAllContainers`
  for `--all`, line 1682) is captured *once* but *called fresh inside
  `computeStats` every single iteration* — so each interval re-lists
  every stored container from scratch, not a fixed set captured at
  the very start.
- `~/git/podman/cmd/podman/containers/stats.go:31`: real podman's own
  doc-comment primary example, `podman stats --all --no-stream`, was
  never the *only* supported form — it's simply the fastest one to
  demonstrate; `podman stats --all` (no `--no-stream`) is equally
  real and, per the source above, equally supported.
- Both `--all` and the plain default (no explicit container, no
  `--latest`) cases are marked `queryAll = true` in real podman
  (`containers.go:1681,1687`), which means a container that vanishes
  between listing and sampling is silently skipped rather than ending
  the whole stream (`computeStats`'s own `errors.Is(err, define.
  ErrCtrRemoved) || ...` skip list, `containers.go:1712-1719`) — this
  project's own [`sample_container_stats`] already has the equivalent
  honest "not running any more" skip built in via its `Ok(None)`
  return, needing no new error-classification logic to match this.

## Real functional gap, not a no-op

Before this, `ociman stats --all` without `--no-stream` was a hard,
immediate "not yet supported" error — there was no way at all to get
a live, continuously-updating view across every container. Live-
verified by hand: ran two real, long-running containers, then `ociman
stats --all --interval 2 --no-reset`, confirming both appear and their
CPU/memory samples genuinely change between intervals; stopped both
containers and confirmed the stream keeps running, printing an
honestly empty table each interval rather than exiting — matching real
podman's own identical "queryAll never ends the stream" behavior;
started a brand-new third container *while the stream was already
running* and confirmed it appeared on the very next sample, proving
the re-listing (not a fixed enumeration captured once) is real.

## Why this is narrow and safe

No new architecture needed: this project already had every piece —
[`sample_container_stats`] (the existing per-container sampler),
`cmd_stats`'s own existing single-container streaming loop shape
(interval sleep + optional screen-clear), and `cmd_stats_all`'s own
existing one-shot multi-container enumeration. This change is purely
a refactor-and-compose: the enumeration-plus-sampling logic factored
out of `cmd_stats_all` into a new, shared `sample_all_container_stats`
helper, then a new `cmd_stats_all_streaming` function wraps that
helper in the exact same interval-sleep loop shape `cmd_stats` already
established, printing via the already-existing `print_stats_samples`.
No cgroup, namespace, capability, systemd, or mount code needed any
changes at all — `sample_container_stats`'s own cgroup-reading
internals are completely untouched.

## Tests

Two new integration tests in `tests/tests/ociman_stats.rs`:
- `stats_all_streaming_reports_repeated_samples_and_never_ends_on_its_own`
  — reads real, live stdout from a spawned `stats --all --interval 1
  --no-reset` process until at least two full samples (two separate
  `CPU %` headers) have been printed, confirms the running container
  appears in them, then confirms the process is *still alive*
  (`try_wait()` returns `None`) before killing it — proving the
  stream is genuinely unbounded, not something that silently exits
  early.
- `stats_all_streaming_picks_up_a_container_created_after_the_stream_started`
  — starts the stream against an empty store, waits for a real, empty
  sample, *then* creates a container, and confirms it appears in a
  later sample — proving the re-listing is real, not a fixed
  enumeration captured once at stream start.

The pre-existing `stats_all_without_no_stream_is_a_clear_not_yet_error`
test (asserting the old "not yet supported" error) is replaced by the
above, since that error no longer exists.

Manually verified end to end beyond the automated tests (see "Real
functional gap" above): real multi-container streaming, empty-table
persistence after every container stops, and live pickup of a
container created mid-stream.

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (129
test-result blocks, all passing on the first attempt with
`RUST_TEST_THREADS=2`), `python3 ci/guards.py` (clean), `cargo deny
check` (clean), `bash ci/native-ci.sh` (clean on the first attempt),
`bash ci/build-deb.sh` (clean on the first attempt, real `dpkg -i`/
`--version`/`dpkg -r` round trip). `ci/bench.sh` was not rerun: this
change is confined to `ociman stats`, a read-only monitoring command
with no effect on container startup, exec, or destroy hot paths —
the same "no bench rerun needed" reasoning `0560`'s own note already
established for this exact command family.

A real, previously-unnoticed leak was found and fixed during test
development, not in the feature itself: an early draft of the new
tests killed only the spawned `ociman run` foreground supervisor
process on cleanup, not the actual container it supervises (a real
`systemd --user` transient scope with its own live processes,
matching this project's own already-established cgroup-driver
architecture) — leaving orphaned live containers behind after the
test process exited. Fixed by using the same `ociman kill <id>` +
`run.wait()` + `ociman rm <id>` cleanup sequence every other test in
this file that runs a long-lived container already establishes.

## Deliberately still out of scope

This closes `0560`'s own last remaining `stats` gap entirely — no
`ociman stats --all` scope restriction remains. Other, unrelated
deferred `stats` candidates (real network/block I/O columns, `--all`
combined with `--latest`/an explicit id, which remains a real,
mutually-exclusive error matching real podman exactly) are untouched
and not part of this note.
