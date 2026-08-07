# Design note 0561: global `--debug`/`-D` flag

Status: implemented
Scope: `crates/oci-cli-common/src/args.rs`, `crates/oci-cli-common/
src/logging.rs`, `tests/tests/smoke.rs`.

## What this closes

None of this project's six binaries accepted `-D`/`--debug` before
this — a real, previously entirely missing flag every one of them
should have, since `oci_cli_common::GlobalArgs`/`logging::init` are
shared verbatim by every binary (the exact same shared-crate
precedent `--json`/`--log-level` themselves already establish).

## Real, checked-directly confirmation

Three of the four real reference tools this project ports each have
their own genuinely real, live-consumed `--debug` flag:

- **podman/docker** (`ociman`'s primary target): `~/git/podman/cmd/
  podman/root.go:716-717` — `lFlags.BoolVarP(&debug, "debug", "D",
  false, "Docker compatibility, force setting of log-level")`. Live
  consumer, `root.go:492-500` (`loggingHook`): if `--debug` is set
  *and* `--log-level` was also explicitly changed away from its own
  default (`"warn"`, `root.go:98-99`), a real, immediate error —
  `"Setting --log-level and --debug is not allowed"` — otherwise
  `logLevel` is forced to `"debug"`.
- **runc**: `~/git/runc/main.go:106-109` — plain `&cli.BoolFlag{Name:
  "debug", ...}`; consumed at `main.go:201-203` (`configLogrus`):
  `if cmd.Bool("debug") { logrus.SetLevel(logrus.DebugLevel) ... }`.
- **crun**: `~/git/crun/src/crun.c:228` — `{ "debug", OPTION_DEBUG,
  ... }`; consumed at `crun.c:291-292`: `arguments.verbosity =
  LIBCRUN_VERBOSITY_DEBUG`.

podman/docker's is the richest (the `-D` short alias plus the
conflict check), the shape worth porting; `ocirun`/`ocicri`/`ocibox`
independently benefit too, since real `runc --debug`/`crun --debug`
are exactly this same underlying idea (force debug-level logging),
just without podman's extra short-alias/conflict-check richness.

## Real functional gap, not a no-op

Real: there was no way at all to get `ociman`'s equivalent of `podman
--debug`/`docker -D`/`runc --debug`/`crun --debug` — a hard clap
parse error before this. Live-verified by hand across every one of
the six binaries (`ociman`/`ocirun`/`ocicri`/`ocibox`/`ociboot`/
`ocivmm`): `--debug`/`-D` alone genuinely forces the `"<bin> starting"`
debug line to appear (suppressed by the default `warn` filter);
`--debug` combined with an explicit, non-default `--log-level` (e.g.
`--log-level=error`) is a real, immediate error with podman's own
exact wording; `--debug --log-level=warn` (the same value as the
default) is accepted, matching real podman's own identical rule
exactly (only an *actually different* value conflicts).

## Why this is narrow

Entirely a pure CLI-parse-time → logging-init-time translation,
executed once at process start before any container/store/runtime
logic runs. Touches exactly one shared struct (`GlobalArgs`) and one
shared function (`logging::init`) in `oci-cli-common`; **zero** of
the six existing `logging::init(&cli.global)` call sites need to
change at all. Nothing is persisted to disk, nothing is re-read later
by `start`/`stop`/`kill`/`exec`/`update`, and no new field is threaded
through any per-container record.

## Implementation

- `GlobalArgs` gains `debug: bool` (`#[arg(short = 'D', long, global
  = true)]`). Deliberately *not* hidden from `--help`, unlike real
  podman's own `MarkHidden("debug")`: this project's own established
  convention documents every flag's real semantics directly in
  `--help` (the field's own doc comment), so hiding it would only
  obscure a real, working flag for no functional reason.
- `logging::init`: a new `DEFAULT_LOG_LEVEL` constant (`"warn"`,
  matching `GlobalArgs::log_level`'s own `default_value` and real
  podman's own identical `defaultLogLevel`). When `args.debug` is
  set, `args.log_level != DEFAULT_LOG_LEVEL` is a real, immediate
  error with podman's own exact wording; otherwise the filter is
  forced to `"debug"` via the already-existing `init_with_filter`.
  The non-`debug` path (the overwhelming common case) is completely
  unchanged — one extra `bool` check, no allocation, no new work.

## Interaction with `ocibox`'s own `0557` `--verbose`

`ocibox enter`/`ephemeral --verbose` (`0557`) bypasses
`logging::init(&cli.global)` entirely in its own `force_debug` case,
calling `init_with_filter("debug")` directly instead — so combining
`--debug` *and* `--verbose` on the same `ocibox enter` invocation
doesn't run the new conflict check (`--verbose` simply wins,
silently, the same way it already did before this increment existed).
A real, narrow, acceptable edge case: the two flags are genuinely
orthogonal (one project-wide global, one command-specific), and
`0557` never established a conflict-check contract to begin with.

## Tests

Three new tests in `crates/oci-cli-common/src/args.rs`/`logging.rs`
(`debug_flag_and_its_short_alias_parse`, `debug_flag_conflicts_with_a_
non_default_log_level`) proving the flag parses and the conflict
check fires without ever reaching `init_with_filter`'s own real
`try_init()` call (unsafe to do unconditionally alongside every other
test in the same process, the same property the existing
`rejects_invalid_filter` test already relies on). Three new real,
end-to-end tests in `tests/tests/smoke.rs`, mirroring the existing
`--json` tests exactly: `debug_flag_and_its_short_alias_are_accepted_
globally`, `debug_flag_conflicts_with_an_explicit_log_level_
everywhere` (a real subprocess spawn per binary, checking the real
error text), and `ocicri_debug_flag_is_accepted_and_the_server_still_
starts` (the same liveness-check pattern the existing `ocicri --json`
test already established, since a bare `ocicri` invocation is real,
valid default behavior — starting the server — not something a
quick `Command::output()` call could observe).

Manually verified end to end across all six binaries, exactly as
described in "Real functional gap" above.

Full workspace: `cargo build --workspace --locked` (clean across
every binary), `cargo fmt --all` (clean), `cargo clippy --workspace
--all-targets --locked -- -D warnings` (clean), `cargo test
--workspace --locked` (128 test-result blocks, all passing —
`RUST_TEST_THREADS=2` given this host's own heavy, persistent
concurrent-session CPU contention this same day), `python3 ci/
guards.py` (clean), `cargo deny check` (clean), `bash ci/native-ci.sh`
(clean on the first attempt using `RUST_TEST_THREADS=2`), `bash ci/
build-deb.sh` (clean on the first attempt, real `dpkg -i`/
`--version`/`dpkg -r` round trip). `bash ci/bench.sh` was re-run in
full (this change touches `logging::init`, called by every binary at
startup, part of the measured hot path): every single comparison
still shows large wins, unaffected — `ocirun run` 1.85x/5.39x faster
than crun/runc; `ociman exec` 10.23x/32.44x faster than docker/
podman; `ociman run --rm` 5.83x/8.03x faster; `ociman rm` 39.56x
faster; `ociman run -d` 3.06x/3.85x faster; `ociman commit` 33.46x
faster; `ociman build --no-cache` 18.90x/23.50x faster; `ociman build`
(cached) 24.43x/33.70x faster — confirming the one extra `bool` check
in the non-`debug` common path adds no measurable overhead.

## Deliberately still out of scope

The `ocibox --verbose`/`--debug` interaction noted above (`--verbose`
silently wins when both are given on `enter`/`ephemeral`) is a real,
narrow, acceptable edge case, not separately closed here.
