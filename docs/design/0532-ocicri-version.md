# Design note 0532: `ocicri version`

Status: implemented
Scope: `bin/ocicri/src/main.rs`, `tests/tests/ocicri_version_cli.rs`.

## What this adds, and a correction

`bin/ocicri/src/main.rs`'s own module doc comment used to claim "real
`cri-o` itself has no subcommands at all — invoking it just *is*
running the server". This turns out to be factually wrong, checked
directly: real `crio` *does* have real subcommands, and this
increment adds this project's own first one, `ocicri version`.

## Real, checked-directly confirmation

- `~/git/cri-o/cmd/crio/main.go:161-168`:
  ```go
  app.Commands = criocli.DefaultCommands
  app.Commands = append(app.Commands,
      criocli.CheckCommand,
      criocli.ConfigCommand,
      criocli.PublishCommand,
      criocli.StatusCommand,
      criocli.VersionCommand,
      criocli.WipeCommand,
  )
  ```
  `app.Action` (line 231) is what runs when *no* subcommand is given —
  the real server start, exactly matching this project's own already-
  existing `Cli::command: Option<Command>` default (`None` starts the
  server, unchanged by this increment).
- `~/git/cri-o/internal/criocli/version.go:17-49` (`VersionCommand`):
  `Usage: "display detailed version information"`, flags `--json`/
  `-j` and `--verbose`/`-v` (Go-module-dependency-list-only, see
  below), `Action` calls `version.Get(verbose)` then prints `v.
  String()` (plain text) or `v.JSONString()` (`--json`).
- `~/git/cri-o/internal/version/version.go:35-49` (`Info` struct):
  `Version`, `GitCommit`, `GitCommitDate`, `GitTreeState`,
  `BuildDate`, `GoVersion`, `Compiler`, `Platform`, `Linkmode`,
  `BuildTags`, `LDFlags`, `SeccompEnabled`, `AppArmorEnabled`,
  `Dependencies` (verbose-only). `Platform` is `fmt.Sprintf("%s/%s",
  runtime.GOOS, runtime.GOARCH)` — the same os/arch concept this
  project's own `oci_spec_types::image::Platform::host()` already
  provides for `ociman version`.
- `~/git/cri-o/internal/version/version.go:228-273` (`(*Info)
  .String()`): a reflection-driven `tabwriter` dump, one `FieldName:
  \tvalue` line per non-empty field, real field names used as-is
  (`Version`, `GitCommit`, `Platform`, ... — no inserted spaces,
  unlike `ociman version`'s own podman-style `"Git Commit"`/`"OS/
  Arch"` labels).

## Implementation

`bin/ocicri/src/main.rs`:
- Corrected the module's own doc comment (see above), and `Cli`'s own
  ("real `cri-o` itself has no subcommands...") — both cited the
  wrong claim, now cite `main.go:161-168` directly instead.
- New `command: Option<Command>` field on `Cli` (previously absent
  entirely), checked right after `logging::init` and before the
  socket-bind/tokio-runtime setup — `None` (still the only way to
  reach that setup) preserves today's exact default behavior
  byte-for-byte; `Some(Command::Version)` returns early via the new
  `cmd_version`, never touching `serve()`/the Unix socket/tokio at
  all.
- New one-variant `enum Command { Version }` — real `crio`'s own
  other subcommands (`check`/`config`/`publish`/`status`/`wipe`) are
  each a real, separate, much bigger gap (a standalone healthcheck-
  config CLI, a config-file generator/validator, a systemd-notify-
  socket publisher, a runtime status dump, an on-disk-state wipe
  tool respectively) — deliberately not folded in here.
- New `VersionReport { version, git_commit, platform }` + `version_
  report()` + `cmd_version(json)`, modeled directly on `ociman
  version`'s own already-established `VersionReport`/`version_
  report()`/`cmd_version()` (same crate versions/git-hash/platform
  primitives, all already dependencies of `ocicri`'s own `Cargo.
  toml` — zero new crates needed). Real crio's own separate `--json`/
  `-j` flag on this one subcommand is folded into this project's
  already-global `--json` instead, matching every other `ocicri`/
  `ociman` command's own identical convention — not a second,
  redundant flag. `--verbose`/`-v` (real crio's own Go-module-
  dependency-list dump) is deliberately not accepted at all: there is
  no Rust-module-list equivalent to honestly populate it with.
- Plain-text output uses real crio's own exact field *names*
  (`Version:`/`GitCommit:`/`Platform:`), but doesn't chase its own
  reflection-driven `tabwriter` column width byte-for-byte — that
  width is computed from *all* of real crio's own fields, most of
  which this report has no honest equivalent for at all (`GoVersion`/
  `Compiler`/`Linkmode`/`BuildTags`/`LDFlags`: not Go;
  `GitCommitDate`/`BuildDate`/`GitTreeState`: no build-time
  timestamp/dirty-tree embedding here; `SeccompEnabled`/
  `AppArmorEnabled`: no seccomp/AppArmor subsystem yet;
  `Dependencies`: `--verbose`-only Go module list) — chasing an exact
  width real crio would only ever produce with fields this project
  can't honestly populate would be cargo-culting, not real
  compatibility. JSON key casing likewise stays this project's own
  established `snake_case` (`ociman version`'s own precedent), not
  real crio's `camelCase` struct tags.

## Tests

Four new integration tests in `tests/tests/ocicri_version_cli.rs` (a
new file, deliberately separate from the pre-existing
`ocicri_version.rs`, which covers the genuinely different
`RuntimeService.Version` gRPC RPC):
- `version_prints_a_real_version_git_commit_and_platform`
- `version_json_emits_version_git_commit_and_platform_fields`
- `version_uses_the_global_json_flag_not_a_local_one`
- `no_subcommand_still_starts_the_real_server` (a real regression
  guard: spawns the actual binary with no subcommand, waits for its
  own real socket file to appear, then kills it — proving the default
  path is completely unaffected, not just inferring it from the diff)

Manually exercised end to end beyond the automated tests: `ocicri
version` (plain text), `ocicri version --json`, `ocicri --help`/
`ocicri version --help`, a bare `ocicri --listen <path>` invocation
confirmed to still bind a real socket, and the installed `/usr/bin/
ocicri --version` (the pre-existing clap auto-flag, unrelated but
confirmed unaffected) during the real `dpkg -i` round trip below.

Full workspace: `cargo build --workspace --locked` (clean), `cargo fmt
--all` (clean after one auto-fix), `cargo clippy --workspace
--all-targets --locked -- -D warnings` (clean), a full `cargo test
--workspace --locked` run (125 test-result blocks, up from 124 with
the new test file, 0 failures, 0 `FAILED` lines — fully clean on the
first attempt), `python3 ci/guards.py` (clean), `cargo deny check`
(clean), `bash ci/native-ci.sh` (clean on the first attempt), `bash
ci/build-deb.sh` (clean on the first attempt, real `dpkg -i`/
`--version`/`dpkg -r` round trip for every binary). Pure CLI-parsing,
pre-serve addition — no hot path touched (the default, no-subcommand
server-start path is provably unaffected, see the regression-guard
test above), no `ci/bench.sh` rerun needed.

## Deliberately still out of scope

Real `crio`'s own `check`/`config`/`publish`/`status`/`wipe`
subcommands (see above) — each a real, separate, future increment.
`--verbose`/`-v` on `version` itself (no honest Rust-module-list
equivalent, see above).
