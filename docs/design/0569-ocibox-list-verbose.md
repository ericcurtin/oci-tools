# Design note 0569: `ocibox list --verbose`

Status: implemented
Scope: `bin/ocibox/src/main.rs`, `tests/tests/ocibox_list_rm.rs`.

## What this closes

`0557`'s own "Deliberately still out of scope" section states
verbatim: *"`create`, `list`/`rm --all`, `export`, `generate-entry`
have no real, live-consumer chain of their own traced yet."* `0558`
closed `export`, `0564` closed `create`, `0568` closed
`generate-entry`. `list` was the very last item still open on that
checklist — no note between `0568` and now touches it. This closes
it, completing the entire `--verbose` rollout `0557` started.

## Real, checked-directly confirmation

- `~/git/distrobox/internal/cli/root.go:76-81`: the real, root-level
  global `--verbose`/`-v` flag, inherited by every subcommand.
- `~/git/distrobox/internal/cli/list.go`: declares only its own local
  `--no-color` flag (lines 21-26); never calls `cmd.Bool("verbose")`
  anywhere in that 56-line file.
- Confirmed as a real, live CLI-parse gap directly: `ocibox list
  --verbose` was a hard clap "unexpected argument" failure before
  this change.

## Traced the entire real chain of custody — genuinely dead upstream

- `~/git/distrobox/pkg/commands/list.go`: an exhaustive
  `grep -n "[Vv]erbose"` finds zero matches at all — no field on any
  options struct even exists to carry the value through.
- The one place a `verbose` bool could still theoretically reach —
  container-manager construction (`root.go:296-317`'s
  `withContainerManager` → `providers.NewPodman(root, sudoCommand,
  verbose, ...)`, `podman.go:27,41-51`) — is confirmed a dead end
  too, the identical finding `0536`/`0564`/`0568` already established
  for `rm`/`create`/`generate-entry --verbose`: `grep -rn
  "\.verbose\b" ~/git/distrobox/pkg/containermanager/` only matches a
  test assertion, never any real command-generation path.
- Confirmed this is **not** `0557`'s own real, live exception:
  `generateEnterCommand`'s own `verbose` parameter (prepending
  `--log-level debug` to the underlying `exec` invocation) is only
  ever reached from `enter`/`ephemeral`'s own separate call path,
  never from `list`.

## Why this is a real, faithful no-op

A genuinely dead upstream flag, exactly like `0536`/`0564`/`0568`'s
`rm`/`create`/`generate-entry --verbose` — not `0557`'s
`enter`/`ephemeral --verbose`, which is a real functional gap this
project closes by forcing its own log filter to `"debug"`. `list` has
no such live target anywhere upstream to translate; it's also a
direct consequence of the same "no running/stopped state, no color
codes" gap `0515`'s own `--no-color` no-op already established for
this exact command — there is nothing in `list`'s own real output a
verbosity level could ever change here.

## Why this is narrow and safe

Pure CLI-parsing acceptance-and-discard, exactly like
`Command::List`'s existing `no_color`/`root` fields already at the
dispatch site. `cmd_list`'s own signature and enumeration logic are
completely untouched. No cgroup, namespace, capability, systemd, or
mount code is anywhere near this change. No short-flag collision:
`List` has no `--volume` (unlike `Create`/`Ephemeral`, where `0536`/
`0557` had to go long-only), so `--verbose`/`-v` gets its short alias
here, matching `no-op`'s own established convention (`generate-entry`,
`0568`) whenever there's no such collision.

## Tests

One new integration test in `tests/tests/ocibox_list_rm.rs`:
`list_verbose_flag_and_its_short_alias_are_accepted_and_behave_identically`
— proves both `--verbose` and `-v` parse and produce byte-identical
output to a plain `list`, following the exact same pattern as the
adjacent `list_no_color_flag_...`/`list_root_flag_...` tests.

Manually verified end to end beyond the automated test: `ocibox list`,
`ocibox list --verbose`, and `ocibox list -v` against a real empty
store all print `no boxes` and exit `0` identically.

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (129
test-result blocks, all passing on the first attempt with
`RUST_TEST_THREADS=2` — no new test file added, so the block count is
unchanged from `0568`), `python3 ci/guards.py` (clean), `cargo deny
check` (clean), `bash ci/native-ci.sh` (one isolated, already-known-
flaky timestamp-boundary test in `ociman_build.rs`,
`build_unsetenv_adds_no_history_entry_of_its_own` — a wall-clock
second-boundary comparison unrelated to this change entirely,
confirmed transient by an immediate isolated rerun, then a fully
clean full rerun), `bash ci/build-deb.sh` (clean on the first
attempt, real `dpkg -i`/`--version`/`dpkg -r` round trip). A pure
CLI-parsing-and-discard addition — no hot path touched, no
`ci/bench.sh` rerun needed.

## Deliberately still out of scope

This closes the entire `--verbose` rollout across every `ocibox`
subcommand (`rm`/`0536`, `enter`/`ephemeral`/`0557`, `export`/`0558`,
`create`/`0564`, `generate-entry`/`0568`, `list`/here) — no
`ocibox`-side `--verbose` gap remains open. Other, unrelated deferred
candidates named in earlier notes (`ocibox create --dry-run`, `0564`;
`ociman stats --all` continuous streaming, `0560`) remain separate,
future candidates.
