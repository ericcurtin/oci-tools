# Design note 0568: `ocibox generate-entry --verbose`

Status: implemented
Scope: `bin/ocibox/src/main.rs`, `tests/tests/ocibox_generate_entry.rs`.

## What this closes

`0557`'s own "Deliberately still out of scope" section states
verbatim: *"`create`, `list`/`rm --all`, `export`, `generate-entry`
have no real, live-consumer chain of their own traced yet."* `0558`
closed `export`, `0564` closed `create`. `generate-entry` was still
open — no note between `0564` and `0567` touches it. This closes it.

## Real, checked-directly confirmation

- `~/git/distrobox/internal/cli/root.go:76-81`: the real, root-level
  global `--verbose`/`-v` flag, inherited by every subcommand.
- `~/git/distrobox/internal/cli/generate-entry.go:16-43` declares no
  local `--verbose` flag of its own (only `delete`/`icon`/`all`);
  line 60 reads the inherited root global directly: `Verbose:
  cmd.Bool("verbose")`.
- Confirmed as a real, live CLI-parse gap directly: `ocibox
  generate-entry somebox --verbose` was a hard clap "unexpected
  argument" failure before this change.

## Traced the entire real chain of custody — genuinely dead upstream

- `~/git/distrobox/pkg/commands/generate_entry.go:35` declares the
  `Verbose` field on `GenerateEntryOptions`, but it is exhaustively
  confirmed never read anywhere else in that same 356-line file
  (`grep -c "Verbose"` finds only the one struct-field declaration
  itself — no second reference).
- The one place a `verbose` bool could still theoretically reach —
  container-manager construction (`root.go:296-317`'s
  `withContainerManager` → `providers.NewPodman(root, sudoCommand,
  verbose, ...)`, `podman.go:27,41-51`) — is confirmed a dead end
  too, the identical finding `0536`/`0564` already established for
  `rm`/`create --verbose`: `grep -rn "\.verbose\b"
  ~/git/distrobox/pkg/containermanager/` only matches a test
  assertion, never any real command-generation path.
- Confirmed this is **not** `0557`'s own real, live exception:
  `generateEnterCommand`'s own `verbose` parameter (prepending
  `--log-level debug` to the underlying `exec` invocation) is only
  ever reached from `enter`/`ephemeral`'s own separate call path,
  never from `generate-entry`.

## Why this is a real, faithful no-op

A genuinely dead upstream flag, exactly like `0536`'s `rm --verbose`
and `0564`'s `create --verbose` — not `0557`'s `enter`/`ephemeral
--verbose`, which is a real functional gap this project closes by
forcing its own log filter to `"debug"`. `generate-entry` has no such
live target anywhere upstream to translate.

## Why this is narrow and safe

Pure CLI-parsing acceptance-and-discard, exactly like
`Command::GenerateEntry`'s existing `all`/`delete`/`icon`/`root`
fields already at the dispatch site. `cmd_generate_entry`'s own
launcher-writing logic is completely untouched. No cgroup, namespace,
capability, systemd, or mount code is anywhere near this change. No
short-flag collision: `all`/`-a`, `delete`/`-d`, `icon`/`-i`,
`root`/`-r` are already used on this command, but `-v` is free (unlike
`Create`/`Ephemeral`, where `0536`/`0557` had to go long-only because
`-v` is already this project's own established alias for `--volume` —
`generate-entry` has no `--volume` flag at all, so `--verbose`/`-v`
gets its short alias here).

## Tests

One new integration test in `tests/tests/ocibox_generate_entry.rs`:
`generate_entry_verbose_flag_and_its_short_alias_are_accepted_and_behave_identically`
— proves both `--verbose` and `-v` parse, for both plain
`generate-entry` and `generate-entry --delete`, and behave exactly
like the flag being absent (a real `.desktop` launcher file is
created/removed either way).

Manually verified end to end beyond the automated test: a real image
built via `ociman build`, a real box created, `ocibox generate-entry
genbox --verbose` (succeeded, real `.desktop` file created) and
`ocibox generate-entry genbox --delete --verbose` (succeeded, file
removed) — both identical to a plain invocation.

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (129
test-result blocks, all passing — this host's own heavy, sustained
concurrent-session CPU contention caused several isolated,
already-known-flaky `ocicri_container.rs` tests to fail across the
first four attempts this same day, each individually confirmed
transient by an immediate isolated rerun before retrying the full
suite; the fifth full-suite attempt, with `RUST_TEST_THREADS=2`, ran
completely clean), `python3 ci/guards.py` (clean), `cargo deny check`
(clean), `bash ci/native-ci.sh` (clean on the first attempt), `bash
ci/build-deb.sh` (clean on the first attempt, real `dpkg -i`/
`--version`/`dpkg -r` round trip). A pure CLI-parsing-and-discard
addition — no hot path touched, no `ci/bench.sh` rerun needed.

## Deliberately still out of scope

`ocibox list`/`rm --all`'s own `--verbose` gap (also named in `0557`'s
own list) remains open, as does `ocibox create --dry-run`/`-d` (named
in `0564`'s own note) — both left as separate, future candidates.
