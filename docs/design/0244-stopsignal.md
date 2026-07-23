# Design note 0244: honor the image's `STOPSIGNAL`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `bin/ocicri/src/container.rs`,
`bin/ocicri/src/runtime_service.rs`, `tests/tests/ociman_stop.rs`,
`tests/tests/ocicri_container.rs`.

## The gap

A Containerfile's `STOPSIGNAL` lands in the image config's
`stop_signal` field (already modeled in `oci_spec_types` — just never
read by anything). Real `docker stop`/`podman stop` send it instead
of `SIGTERM`; real cri-o's `StopContainer` does the same
(`Container::StopSignal`, checked directly: parsed from the image
config, with a **garbage-tolerant TERM fallback** for an unparsable
declaration rather than a failed stop). Both of this project's
stoppers hardcoded `TERM` — 0238 explicitly deferred the CRI side.

## `ociman stop` (and `restart`)

`--signal` becomes `Option<String>`: an explicit value always wins
(docker's own `stop --signal` semantics); otherwise the image's
declared `STOPSIGNAL` is resolved — via the container state's own
`io.oci-tools.image` annotation, through the same shared
`oci_store::resolve_by_reference_or_id` everything else uses — else
`TERM`. Resolution is deliberately never an error: a stop must work
even when the image was since removed (real podman copies the signal
onto its container record at create time for the same reason; this
project's state schema predates 0244 and reads the image instead —
same observable behavior while the image exists, TERM after an
`rmi`, noted honestly). An unparsable declared value warns and falls
back to TERM, cri-o's own tolerance. `ociman restart`'s internal stop
previously hardcoded `"TERM"` — now `None`, so it honors
`STOPSIGNAL` too (matching real `podman restart`). `ociman kill` is
deliberately untouched: its default is `KILL` and `STOPSIGNAL` plays
no part there (checked against real `podman kill`).

One real refactoring hazard caught while making the change: the
original `signal::parse` two-liner existed verbatim in *two*
functions (`stop_container` and `cmd_kill`), and a blanket
find-and-replace broke `cmd_kill` before the compiler caught it —
the fix was applied to exactly the one intended site.

## `ocicri StopContainer`

`ContainerRecord.stop_signal` (serde-default) captures the image's
declared value at `CreateContainer` (the same moment the image config
is already read for the bundle spec — no extra store round trip);
the graceful phase parses it at stop time with the same TERM
fallback. Pre-0244 records simply have `None` = TERM. The
`ContainerStatus.stop_signal` proto enum stays at its default —
matching real cri-o, whose own status response doesn't populate it
either (checked directly, `server/container_status.go`).

## Verified

- `ociman`: a container whose image declares `STOPSIGNAL SIGUSR1`
  installs *two* traps with distinct exit codes (USR1→43, TERM→21);
  a bare `ociman stop` yields 43 (STOPSIGNAL honored), an explicit
  `--signal TERM` yields 21 (override wins). All pre-existing
  stop/restart/kill tests pass unmodified.
- `ocicri`: the same two-trap container via CRI exits 43 after
  `StopContainer` (with 0238's own pre-exec-race `touch /ready`
  guard).
- Full workspace: `cargo build`, `cargo test --workspace`,
  `cargo fmt --all --check`, `cargo clippy --workspace --all-targets
  -- -D warnings`, `python3 ci/guards.py`, `cargo deny check`,
  `bash ci/native-ci.sh`, `ci/build-deb.sh`, `ci/bench.sh` sanity
  (stop paths only; container startup untouched — destroy timing
  re-checked by the bench run as always).
