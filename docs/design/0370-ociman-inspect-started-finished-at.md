# Design note 0370: `ociman inspect`'s own `started_at`/`finished_at` fields

Status: implemented
Scope: `crates/oci-runtime-core/src/state.rs`, `bin/ociman/src/main.rs`,
`tests/tests/ociman_inspect.rs`, `README.md`.

## What this closes

`PersistedState` (shared by `ocirun` and `ociman`) tracked `created`
but had no `started_at`/`finished_at` at all. Real `podman inspect`'s
own `InspectContainerState.StartedTime`/`FinishedTime`
(`~/git/podman/libpod/container.go`) drive both its own inspect output
and `podman ps`'s "Up 3 minutes"/"Exited ... ago" display — a real,
previously-missing piece of container lifecycle bookkeeping this
project had no equivalent of anywhere.

## Real, checked-directly semantics

Read `~/git/podman/libpod/runtime_ctr.go`/`oci_conmon_common.go`
directly: `StartedTime`/`FinishedTime` are each unconditionally
overwritten (`ctr.state.StartedTime = time.Now()`,
`c.state.FinishedTime = time.Now()`) on every real start/exit — not
just recorded once at the very first start, so a restarted
container's own `StartedTime` always reflects its *most recent* real
start.

## Where these actually get recorded — one real, existing reap point

`ociman`'s own `run_and_finalize` already has exactly the two moments
these need: `record_running` (called once the container's own process
is confirmed alive, right before `run_reporting_pid` blocks on it —
used identically by `run`, `run -d`, and `create`+`start`/`restart`,
since every one of those funnels through this same function) is where
`started_at` is now set; the function's own finalize step (where
`ANNOTATION_EXIT_CODE` already gets recorded, the *only* place
`Status::Stopped` is ever written at all — confirmed by grep, both
sites already updated together) is where `finished_at` is now set,
computed once so both of that step's own two branches (`--rm`'s
already-removed-by-the-time-we-get-here race vs. the ordinary path)
record the identical instant.

## Implementation

New `PersistedState::started_at`/`finished_at: Option<String>`
(RFC3339 UTC, the same `format_rfc3339_utc` helper/precision
`created` already uses) — both `#[serde(default, skip_serializing_if
= "Option::is_none")]`, the same forward-compatible-record convention
`owner` already established, so a `state.json` predating this field
deserializes cleanly. Surfaced in `ContainerInspectView` as
`started_at`/`finished_at` (plain lowercase, matching this project's
own already-established field-naming convention for this view rather
than porting podman's own PascalCase names).

`ocirun`'s own `StateView`/`ocirun state` output is deliberately
**not** touched at all — real `runc state`'s own JSON shape has no
`startedAt`/`finishedAt` concept whatsoever (this is a podman-level
concept, not part of the OCI runtime-spec's own `state.json` shape),
so there is nothing for `ocirun` to expose here; the two new fields
exist purely as shared, optional `PersistedState` storage that only
`ociman` currently populates or reads.

## Verified

New tests in `tests/tests/ociman_inspect.rs`:
`inspect_started_at_and_finished_at_are_absent_for_a_never_started_
container`; `inspect_started_at_and_finished_at_are_set_after_a_
container_runs_to_completion` (`finished_at` always at or after
`started_at`, RFC3339 strings sorting lexically the same as
chronologically); `inspect_restart_overwrites_started_at` (a real
`ociman restart`, waiting past `format_rfc3339_utc`'s own
second-level precision, confirms the value genuinely changes rather
than staying pinned to the very first start). New unit test in
`crates/oci-runtime-core/src/state.rs`:
`create_then_load_round_trips` extended to assert both fields default
to `None`. All 210 pre-existing `oci-runtime-core` unit tests and 15
pre-existing `ociman_inspect.rs` tests re-run unmodified and still
pass.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures, full clean
run, no flakes), `python3 ci/guards.py`, `cargo deny check`, `bash
ci/native-ci.sh`, `bash ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip). This change touches `run_and_finalize`, a real
hot path (`run`/`run -d`/`commit` all exercise it) — `bash
ci/bench.sh` re-run specifically for those three comparisons: `run
--rm` 34.6ms, `run -d` 40.7ms, `commit` 3.6ms, all unchanged from the
`0367`-era baseline (32.3ms/37.6ms/3.5ms) within normal session
noise — the two extra `format_rfc3339_utc` calls plus the (already-
happening) state write cost nothing measurable.
