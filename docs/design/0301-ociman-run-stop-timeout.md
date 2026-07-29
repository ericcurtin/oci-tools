# Design note 0301: `ociman run`/`create --stop-timeout`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_stop.rs`.

## Closing `0300`'s own "still ahead"

`0300` gave `ociman run`/`create` a `--stop-signal` override but
explicitly named `--stop-timeout` as a separately-scoped candidate:
real `docker run --stop-timeout`/`podman run --stop-timeout` (checked
directly against an installed `podman 4.9.3`/`docker 29.2.1`) let you
set a container's own default grace period at creation time, used by
every later `stop`/`restart` given no `--time` of its own.

## A real, checked-directly CLI-level precedence subtlety

Read real podman's own source directly rather than assuming symmetry
with `0300`'s `--stop-signal` precedence: `~/git/podman/cmd/podman/
containers/stop.go`/`restart.go` only ever set `stopOptions.Timeout`
(a `*uint`) when `cmd.Flag("time").Changed` is true — i.e. real
`podman stop`/`podman restart` genuinely distinguish "the user typed
`--time`/`-t`" from "the user didn't", rather than always passing
*some* value (their own `--time` flag's own displayed default of `10`
is cosmetic CLI help text, not what's actually sent when omitted).
`~/git/podman/pkg/domain/infra/abi/containers.go`'s own
`containerStopImpl` then only overrides `c.StopTimeout()` (the
persisted, per-container value from `--stop-timeout`, defaulting to
`10` if that was never given either) when `options.Timeout != nil`.

This project's own `Command::Stop`/`Command::Restart` previously had
`time: u64` with `default_value_t = 10` — clap's own default meant
there was no way to tell "omitted" from "explicitly `--time 10`" at
all, which would have made a persisted `--stop-timeout` unreachable in
practice (the CLI default would always "win"). Fixed by changing both
to `time: Option<u64>` (no clap default) — omitting `--time` now
genuinely produces `None`, letting the real precedence chain resolve
it instead.

## Implementation

One new `--stop-timeout <SECONDS>` flag on the shared `RunArgs`
struct (covers both `run`/`create`). No eager validation needed
(clap's own `u64` type already rejects anything but a plain
non-negative integer at parse time). Persisted verbatim as a new
`ANNOTATION_STOP_TIMEOUT`.

A new `resolve_stop_timeout(state, explicit: Option<u64>) -> u64`
mirrors `0300`'s own `resolve_stop_signal` shape exactly: explicit
`--time` given to *this* `stop`/`restart` call, else the persisted
`--stop-timeout` override, else `10`. `stop_container` (shared by
`cmd_stop`/`cmd_restart`) now resolves `time_secs` through this one
function right after loading `state` — both commands get the new
precedence level automatically.

Also surfaced in `ociman inspect`'s own `ContainerInspectView` as a new
`stop_timeout` field, matching real `podman inspect --format
'{{.Config.StopTimeout}}'` in spirit.

## Verified

Manual, end-to-end (real seeded busybox image, a container with `trap
'' TERM` so it never exits gracefully): `ociman run -d --stop-timeout
2 ...` followed by a bare `ociman stop` (no `--time`) escalated to
`KILL` in ~2.9s (not the plain 10s default) — the persisted value, not
the default, genuinely governed the wait. `ociman run -d
--stop-timeout 30 ...` followed by `ociman stop --time 1` escalated in
~1.9s — an explicit `--time` still overrides the persisted value in
the other direction. `ociman inspect` shows `"stop_timeout": 10` with
no `--stop-timeout` given at all, `30` when given `--stop-timeout 30`.

Integration (`tests/tests/ociman_stop.rs`, 2 new tests):
`run_stop_timeout_is_honored_when_stop_gives_no_explicit_time` — a
persisted `--stop-timeout 1` against a `TERM`-ignoring container is
force-killed well under the plain 10s default window, proven via
elapsed wall-clock time plus the real `137` (`SIGKILL`) exit code;
`stop_explicit_time_overrides_the_persisted_stop_timeout` — an
explicit `stop --time 1` still wins over a persisted `--stop-timeout
60`.

Regression: all 9 `ociman_stop.rs` tests pass (7 pre-existing + 2
new); full `cargo test --workspace --locked` (111 test result blocks,
0 failures).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).

Performance: this touches `ociman run`/`create`'s own hot path only by
adding one more `Option`-gated annotation insert (skipped entirely
when `--stop-timeout` isn't given, the overwhelmingly common case);
`stop`/`restart` gain one extra `BTreeMap` lookup plus a `str::parse`
inside `resolve_stop_timeout`, negligible next to the existing
multi-hundred-millisecond signal/poll loop those commands already run.
No new I/O, no new allocation on the common path; no re-benchmark
needed.

## Still ahead

No further stop-signal/stop-timeout-related gap is known between
`ociman run`/`create`/`stop`/`restart` and real `podman`/`docker`'s
own equivalents.
