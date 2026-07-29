# Design note 0300: `ociman run`/`create --stop-signal`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_stop.rs`.

## The gap

`ociman stop --signal` (an explicit, per-invocation override) and
`0244`'s image-declared `STOPSIGNAL` resolution already existed, but
`ociman run`/`create` had no way to set a container's own stop signal
*at creation time* — real `docker run --stop-signal`/`podman run
--stop-signal` (checked directly against an installed `podman
4.9.3`/`docker 29.2.1`) persist exactly this kind of override on the
container record itself, used by every later `stop`/`restart` given no
`--signal` of its own.

## Implementation

One new `--stop-signal <SIGNAL>` flag on the shared `RunArgs` struct
(`Command::Run`/`Command::Create` both flatten it, so this one edit
covers both, matching every other `RunArgs` flag). Validated eagerly,
right at the top of `prepare_container`, via the same `oci_runtime_
core::signal::parse` `ociman stop --signal` itself already uses — a
typo'd signal name now fails the `run`/`create` outright instead of
only surfacing at the first real `stop`, matching real podman's own
checked-directly behavior (`~/git/podman/pkg/specgen/generate/
container.go` calls `signal.ParseSignalNameOrNumber` at spec-generation
time, before the container is ever created).

Persisted verbatim (the user's own string, e.g. `SIGUSR1`) as a new
`ANNOTATION_STOP_SIGNAL`, following the exact same "insert into the
annotations map, read back later" pattern `ANNOTATION_NAME`/
`ANNOTATION_LABELS` already established.

`stop_signal_from_image` (0244) is unchanged; a new `resolve_stop_
signal` wraps it with the full, real precedence order, checked
directly against `docker stop`/`podman stop`:

1. An explicit `--signal` given to *this* one `stop`/`restart` call.
2. A persisted `run`/`create --stop-signal` override (this note).
3. The resolved image's own declared `STOPSIGNAL` (0244).
4. `TERM`.

`stop_container` (shared by `cmd_stop`/`cmd_restart`) now calls this
one function instead of duplicating the fallback chain inline — both
commands get the new precedence level automatically, no separate wiring
needed. The existing "a declared-but-unparsable value falls back to
TERM with a warning" tolerance (matching real cri-o's own `StopSignal()`
behavior) still applies, but can now only ever be reached via the
image's own STOPSIGNAL — a persisted `--stop-signal` is already
guaranteed parsable by the eager validation above.

Also surfaced in `ociman inspect`'s own `ContainerInspectView` as a new
`stop_signal` field (the fully-resolved effective value, via `resolve_
stop_signal(state, None)`), matching real podman's own `podman inspect
--format '{{.Config.StopSignal}}'` in spirit (real podman normalizes
that field to a bare signal number internally; this project keeps the
user's own original string form instead — a cosmetic difference, not a
functional one, and consistent with `stop_signal_from_image`'s own
existing convention of never renormalizing the image's declared value
either).

## Verified

Manual, end-to-end (real seeded busybox image): `ociman create --name
sigtest --stop-signal SIGUSR1 ...` then `ociman inspect sigtest` shows
`"stop_signal": "SIGUSR1"`; `ociman create --stop-signal NOTASIGNAL
...` fails immediately with a clear error, no container created.
`ociman run -d --stop-signal SIGUSR1 ...` against a real container with
a `trap 'echo GOT_USR1; exit 42' USR1` script, followed by `ociman
stop`, produced exit code 42 and logged `GOT_USR1` — the signal really
was delivered, not just recorded.

Integration (`tests/tests/ociman_stop.rs`, 2 new tests):
`run_stop_signal_overrides_the_images_declared_stopsignal` — a
`--stop-signal SIGUSR2` override wins over the image's own declared
`STOPSIGNAL` (`SIGUSR1`), proven via distinct trap exit codes (62 vs.
43), and is visible via `ociman inspect`'s new field;
`run_with_an_unparsable_stop_signal_fails_fast_and_creates_nothing` —
an invalid value is a clear upfront error with no container left
behind at all (`ps -a` empty afterward).

Regression: all 7 `ociman_stop.rs` tests pass (5 pre-existing + 2 new);
full `cargo test --workspace --locked` (111 test result blocks, 0
failures — one `ociman_logs.rs` test flaked once under full parallel
load, a known class of flake under this project's own established
ritual, confirmed non-regressing via isolated re-run + a full clean
re-run afterward).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).

Performance: this touches `ociman run`/`create`'s own hot path only by
adding one `Option::is_none()`-guarded branch in `prepare_container`
when `--stop-signal` isn't given (the overwhelmingly common case) —
no new I/O, no new allocation, unlike `0298`'s own real added file
copy; no re-benchmark needed.

## Still ahead

`--stop-timeout` (a `run`/`create`-time default for the grace period
`stop`/`restart --time` already accepts per-invocation) is a real,
separately-scoped candidate real `docker run`/`podman run` both
support — not implemented here, kept small and single-purpose to match
this note's own scope.
