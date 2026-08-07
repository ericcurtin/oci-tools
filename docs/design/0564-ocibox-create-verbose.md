# Design note 0564: `ocibox create --verbose`

Status: implemented
Scope: `bin/ocibox/src/main.rs`, `tests/tests/ocibox_create.rs`.

## What this closes

`docs/design/0557`'s own "Deliberately still out of scope" section
states verbatim: *"`--verbose` on every other `ocibox` subcommand
besides `rm` (`0536`) and `enter`/`ephemeral` (here) remains
untouched — `create`, `list`/`rm --all`, `export`, `generate-entry`
have no real, live-consumer chain of their own traced yet."*
`0558` closed the `export` half of that list. `create` was still
open — no note between `0558` and `0563` touches it. This closes it.

## Real, checked-directly confirmation

- `~/git/distrobox/internal/cli/root.go:76-81`: the real, root-level
  global `--verbose`/`-v` flag, inherited by every subcommand.
- `~/git/distrobox/internal/cli/create.go:165-169`: `create`
  additionally **re-declares its own local** `verbose`/`v` flag
  (unlike `rm.go`, which — per `0536`'s own note — declares no local
  copy and relies purely on the inherited global one).
- Confirmed as a real, live CLI-parse gap directly: `ocibox create
  --verbose --image ... --name ...` was a hard clap "unexpected
  argument" failure before this change.

## Traced the entire real chain of custody — genuinely dead upstream

- `create.go`'s own `createAction` body never reads `cmd.Bool
  ("verbose")` anywhere (exhaustive `grep 'Bool("verbose")'` across
  `~/git/distrobox/internal/cli/` and `~/git/distrobox/pkg/`: only
  `enter.go`/`rm.go`/`generate-entry.go`/`root.go` match — `create.go`
  is absent).
- Not forwarded into `commands.CreateOptions{...}`
  (`~/git/distrobox/internal/cli/create.go:200-224` — no `Verbose`
  field in that literal), nor into the `GenerateEntryOptions{...}`
  call (`~/git/distrobox/pkg/commands/create.go:163-176`).
- The one place a `verbose` bool could still theoretically reach —
  container-manager construction (`root.go:296-317`'s
  `withContainerManager` → `providers.NewPodman(root, sudoCommand,
  verbose, ...)`, `podman.go:27,41-51`) — is confirmed a dead end
  too: `grep -rn "\.verbose\b" ~/git/distrobox/pkg/containermanager/`
  only matches a test assertion (`providers/clone_internal_test.go`),
  never any real command-generation path. This is `0536`'s own
  already-precedented finding for `rm --verbose`, holding identically
  here for `create`.
- Confirmed this is **not** `0557`'s own real, live exception:
  `generateEnterCommand`'s own `verbose` parameter (`podman.go:
  809-823`, prepending `--log-level debug` to the underlying `exec`
  invocation) is only ever reached from `enter`/`ephemeral`'s own
  separate call path, never from `create`.

## Why this is a real, faithful no-op

A genuinely dead upstream flag, exactly like `0536`'s `rm --verbose`
— not `0557`'s `enter`/`ephemeral --verbose`, which is a real
functional gap this project closes by forcing its own log filter to
`"debug"` (since that flag's own real target is a live-consumed,
shelled-out `--log-level debug` this project has no equivalent
mechanism to replicate beyond its own tracing output). `create` has
no such live target anywhere upstream to translate.

## Why this is narrow and safe

Pure CLI-parsing acceptance-and-discard, exactly like `Command::
Create`'s existing `yes`/`no_entry`/`root`/`absolutely_disable_root_
password_i_am_really_positively_sure` fields already at the dispatch
site. `cmd_create`'s own signature and `create_box`'s real pull/
extract logic are completely untouched. No cgroup, namespace,
capability, systemd, or mount code is anywhere near this change.

## A real design decision already flagged by this project itself

`0536` already found and explicitly avoided this exact collision:
*"no short-flag collision on `Rm` — unlike `Create`/`Ephemeral`,
where `-v` is already this project's own established convenience
alias for `--volume`."* `Command::Create`'s own `volume` field
already owns `short = 'v'`. `0557` hit the identical collision for
`Ephemeral` and resolved it the same way: long-only `--verbose`, no
short alias. This note follows the identical, already-established
resolution for `create`.

## Tests

One new integration test in `tests/tests/ocibox_create.rs`:
`create_verbose_flag_is_accepted_and_behaves_identically` — proves
`--verbose` parses and a box is still created exactly as a plain
`create` would.

Manually verified end to end beyond the automated test: a real image
built via `ociman build`, `ocibox create --image ... --name ...
--verbose` confirmed to succeed identically to a plain `create`.

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (128
test-result blocks, all passing — no new test file added, so the
block count is unchanged from `0563`; `RUST_TEST_THREADS=2` given
this host's own heavy, persistent concurrent-session CPU contention
this same day), `python3 ci/guards.py` (clean), `cargo deny check`
(clean), `bash ci/native-ci.sh` (clean on the first attempt), `bash
ci/build-deb.sh` (clean on the first attempt, real `dpkg -i`/
`--version`/`dpkg -r` round trip). A pure CLI-parsing-and-discard
addition — no hot path touched, no `ci/bench.sh` rerun needed.

## Deliberately still out of scope

Real `distrobox create --dry-run`/`-d` ("only print the container
manager command generated") is a genuinely different, real,
live-consumed flag (checked directly, `~/git/distrobox/internal/cli/
create.go:223`: `DryRun: cmd.Bool("dry-run")`, threaded through
several real branches of `pkg/commands/create.go`'s own `createAction`
— pull-skip, entry-generation-skip, clone-validation-skip). A
genuinely bigger feature than this note's own narrow no-op scope,
left as a separate, future candidate. `list`/`generate-entry`'s own
`--verbose` gaps (also named in `0557`'s own list) remain open too.
