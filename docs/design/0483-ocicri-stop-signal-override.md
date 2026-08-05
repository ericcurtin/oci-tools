# Design note 0483: `ocicri` `ContainerConfig.stop_signal` override

Status: implemented
Scope: `bin/ocicri/src/runtime_service.rs`,
`tests/tests/ocicri_container.rs`.

## What this closes

`ocicri` only ever honored the image's own declared `STOPSIGNAL`
(`0244`) — the CRI request's own explicit, per-container
`ContainerConfig.stop_signal` override field was read nowhere at all,
silently dropped every time a client actually set it.

## Real, checked-directly confirmation

- `~/git/cri-o/server/container_create.go:1585-1591`
  (`setupContainerRuntimeAndStopSignal`):
  ```go
  // Determine the stop signal for the container. If a custom stop
  // signal is provided via CRI API, use it. Otherwise, fall back to
  // the image's default stop signal as defined in its configuration.
  // https://github.com/kubernetes/enhancements/issues/4960
  stopSignal = containerImageConfig.Config.StopSignal
  if signal := ctr.Config().GetStopSignal(); signal != types.Signal_RUNTIME_DEFAULT {
      log.Debugf(ctx, "Override stop signal to %s", signal)
      stopSignal = signal.String()
  }
  ```
  The cited KEP (`kubernetes/enhancements#4960`) confirms this is a
  real, intentional, upstream-tracked feature, not incidental
  behavior.
- `~/git/cri-o/internal/factory/container/container.go:397`:
  `func (c *container) Config() *types.ContainerConfig` confirms
  `ctr.Config()` is literally the CRI request's own `ContainerConfig`
  — `.GetStopSignal()` is the generated accessor for proto field 18.
- `crates/oci-cri-types/proto/api.proto:1295`: `Signal stop_signal =
  18;` on `ContainerConfig`, confirming the field is already modeled
  in the vendored proto — this project's own generated Rust already
  has it, just never read.

## Implementation

A small, purely additive change at the one place `ContainerRecord.
stop_signal` was populated (`create_container`, `runtime_service.rs`):

```rust
stop_signal: cri::Signal::try_from(config.stop_signal)
    .ok()
    .filter(|signal| *signal != cri::Signal::RuntimeDefault)
    .map(|signal| signal.as_str_name().to_string())
    .or_else(|| image_config.stop_signal.clone().filter(|s| !s.is_empty())),
```

- `cri::Signal::try_from` is prost's own auto-derived `TryFrom<i32>`
  (`#[derive(::prost::Enumeration)]`) — no new dependency.
- `RUNTIME_DEFAULT` (`0`, the proto's own documented "not specified"
  value, and what an omitted field always deserializes to) means no
  override was given — falls straight through to the image's own
  `STOPSIGNAL`, the exact pre-existing `0244` behavior, unchanged.
- `.as_str_name()` produces the identical `SIGTERM`/`SIGUSR1`-shaped
  string this field's own `Option<String>` already stores, and
  `oci_runtime_core::signal::parse` already consumes unmodified at
  `StopContainer` time — zero changes needed to the stop-time
  consumer, the storage schema, or `oci_runtime_core` itself.

## Tests

Two new integration tests in `tests/tests/ocicri_container.rs`:
`stop_container_honors_an_explicit_cri_stop_signal_override` (an
explicit `SIGUSR2` request wins over the image's own declared
`SIGTERM`, proven via a distinct USR2-trap exit code, 44, never
confusable with the pre-existing USR1-trap test's own 43),
`stop_container_with_the_default_cri_stop_signal_falls_back_to_the_
images_stopsignal` (the real, default `RUNTIME_DEFAULT` value — first
asserted directly against `container_config()`'s own real default,
not assumed — still falls through to the image's own `STOPSIGNAL`
unchanged, a real regression guard for the pre-existing `0244`
behavior this increment must never break). All 41 tests in the file
pass (39 prior + 2 new).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (122 test-result
blocks, 0 failures — run with `--test-threads=4` due to this dev
host's own unusually heavy concurrent load this session, the same
already-documented long-running CPU-spinning background process plus
a second, independent `opencode` agent process; every individual
flaky failure hit along the way was independently confirmed passing
instantly in isolation first), `python3 ci/guards.py` (clean), `cargo
deny check` (clean), `bash ci/native-ci.sh` (clean, 122/122 on the
first attempt), `bash ci/build-deb.sh` (clean, real `dpkg -i`/
`--version`/`dpkg -r` round trip on the first attempt). No benchmark
re-run needed: `ci/bench.sh` never exercises `ocicri` at all, and
this touches container-metadata resolution at create time, not any
launch-mechanism hot path.
