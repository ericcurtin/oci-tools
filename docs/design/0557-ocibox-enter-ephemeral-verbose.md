# Design note 0557: `ocibox enter`/`ephemeral --verbose`/`-v`

Status: implemented
Scope: `bin/ocibox/src/main.rs`, `tests/tests/ocibox_enter.rs`,
`tests/tests/ocibox_ephemeral.rs`.

## What this closes

Real `distrobox` has a root-level, inherited `--verbose`/`-v` global
flag every subcommand accepts. `docs/design/0536` already added it to
`ocibox rm` as a real, faithful no-op — but its own note explicitly
traced the flag's entire chain of custody and found **one** real,
live exception it deliberately scoped out: `enter`. This closes that
gap for `enter` and its `ephemeral` sibling (which inherits every
`create` flag, `--verbose` included).

## Real, checked-directly confirmation

- `~/git/distrobox/internal/cli/root.go:76-81`: the flag's own real,
  root-level registration, inherited by every subcommand.
- `~/git/distrobox/internal/cli/enter.go:125`: `Verbose: cmd.Bool
  ("verbose")` on the constructed `EnterOptions`.
- `~/git/distrobox/pkg/containermanager/providers/podman.go:809-823`
  (`docker.go:729,734-735` identically): `generateEnterCommand`'s own
  `verbose bool` parameter — `if verbose { cmd = append(cmd,
  "--log-level", "debug") }`, prepended before `exec`/`--interactive`/
  `--detach-keys=`. This is the live consumer `0536` already found and
  deliberately scoped out of its own `rm`-only note: real distrobox's
  `--verbose` makes the underlying `podman exec ...` invocation
  `enter` actually runs, run with `--log-level debug`.
- Traced the call site directly: `Podman.Enter`
  (`~/git/distrobox/pkg/containermanager/providers/podman.go:521-538`)
  passes `options.Verbose` straight into `generateEnterCommand`.

## Why this is a real functional gap, not a faithful no-op

Unlike every other consumer `0536` traced (all genuinely dead code in
real distrobox itself), this one is live. This project never shells
out to a real `podman`/`docker` binary at all (a from-scratch
reimplementation, `oci_runtime_core::launch` directly), so there is
no such invocation to prepend a flag onto — but it already has the
exact same real intent's own general mechanism, already wired into
every binary including this one:
[`oci_cli_common::args::GlobalArgs::log_level`]
(`crates/oci-cli-common/src/args.rs`), a `tracing_subscriber::
EnvFilter` string defaulting to `"warn"`. Forcing that filter to
`"debug"` for this one invocation is a real, checked-directly honest
behavior change — turning up this process's own real log output
(this project already emits `tracing::debug!("ocibox starting")`
right after init, plus other debug-level instrumentation elsewhere)
— the same practical effect upstream's flag has, through this
project's own already-real mechanism rather than a shelled-out flag
it has no equivalent of.

Unconditional, matching real distrobox's own identical unconditional
override: an explicit `--log-level` also given loses, exactly like
upstream's own freshly-appended `--log-level debug` always wins
regardless of whatever else was already configured on the underlying
`podman`/`docker` invocation.

## Why this is narrow

Fully contained to `main()`'s own one-time startup sequence and two
sibling struct definitions — no persisted state, no lifecycle reload
sites, no other command needs to know about it:

- `Command::Enter` gains `verbose: bool` (`#[arg(long, short = 'v')]`
  — no collision on `Enter`, unlike `Create`/`Ephemeral`).
- `Command::Ephemeral` gains the identical field, but with **no**
  `-v` short alias (`-v` is already this project's own established
  convenience alias for `--volume` there, the same real collision
  `0536` already found and worked around).
- In `main()`, before calling `oci_cli_common::logging::init`, a
  `matches!` peek at the already-parsed `cli.command` (borrowed, not
  moved) decides between `logging::init_with_filter("debug")` (when
  `Enter { verbose: true, .. }`/`Ephemeral { verbose: true, .. }`) and
  the existing `logging::init(&cli.global)` otherwise.
- Both dispatch arms simply add `verbose: _` to their existing
  destructure — `cmd_enter`/`cmd_ephemeral`'s own signatures are
  unchanged.

## Tests

Three new integration tests: `enter_verbose_flag_and_its_short_alias_
force_debug_level_logging` and `enter_verbose_overrides_an_explicit_
conflicting_log_level` in `tests/tests/ocibox_enter.rs`, and
`ephemeral_verbose_flag_forces_debug_level_logging` in `tests/tests/
ocibox_ephemeral.rs` — each asserts the `"ocibox starting"` debug
line is genuinely absent by default and genuinely present (on
stderr) with `--verbose`/`-v`, including overriding an explicit,
conflicting `--log-level error`.

Manually verified end to end beyond the automated tests: a real image
built via `ociman build`, a real box created from it, `ocibox enter
--verbose`/`-v` and `ocibox ephemeral --verbose` each confirmed to
print the debug line a plain invocation suppresses, including with
an explicit `--log-level=error` still losing to `--verbose`.

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (128
test-result blocks, all passing — a single, isolated
`ocicri_container.rs` cgroup test flaked once under this host's own
concurrent-session CPU contention, confirmed transient by an
immediate isolated rerun, then a fully clean full-suite rerun),
`python3 ci/guards.py` (clean), `cargo deny check` (clean), `bash
ci/native-ci.sh` (clean on the first attempt), `bash ci/build-deb.sh`
(clean on the first attempt, real `dpkg -i`/`--version`/`dpkg -r`
round trip). `ci/bench.sh` was not rerun: it has no `ocibox`
benchmark at all (only `ocirun`/`ociman`), and this change adds a
single `matches!` check against an already-parsed enum before
logging init — no measurable overhead on any hot path.

## Deliberately still out of scope

`--verbose` on every other `ocibox` subcommand besides `rm` (`0536`)
and `enter`/`ephemeral` (here) remains untouched — `create`,
`list`/`rm --all`, `export`, `generate-entry` have no real,
live-consumer chain of their own traced yet, and adding it
speculatively everywhere without that same checked-directly tracing
would risk a fabricated, non-upstream-matching behavior.
