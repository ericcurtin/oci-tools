# Design note 0565: `ocicri publish`

Status: implemented
Scope: `bin/ocicri/src/main.rs`, `tests/tests/ocicri_publish.rs`.

## What this closes

`docs/design/0532` and `bin/ocicri/src/main.rs`'s own enum doc
comment (before this change) both listed real crio's remaining
subcommands (`check`/`config`/`publish`/`status`) as "real, separate,
much bigger gaps" — `publish` specifically described as "a systemd-
notify-socket publisher." That description is checked-directly
wrong (see below), and no note between `0542` (`wipe`) and `0564`
ever revisited or closed it. This adds `ocicri publish` and corrects
the mischaracterization.

## Real, checked-directly confirmation

- `~/git/cri-o/cmd/crio/main.go:161-168`: `app.Commands =
  criocli.DefaultCommands`, then `append(..., criocli.CheckCommand,
  criocli.ConfigCommand, criocli.PublishCommand,
  criocli.StatusCommand, ...)` — `publish` is a real, registered
  top-level `crio` subcommand.
- `~/git/cri-o/internal/criocli/publish.go:7-25` — the whole command
  definition:
  ```go
  var PublishCommand = &cli.Command{
      Name:  "publish",
      Usage: "receive shimv2 events",
      Flags: []cli.Flag{
          &cli.StringFlag{Name: "topic", Hidden: true},
          &cli.StringFlag{Name: "namespace", Hidden: true},
      },
      HideHelp: true,
      Hidden:   true,
      Action: func(c *cli.Context) error { return nil },
  }
  ```
  Two string flags (`--topic`, `--namespace`), the command itself
  hidden from real crio's own `--help`, and an `Action` that
  unconditionally `return nil`s — never reads either flag, never
  reads stdin, never dials anywhere.

## Correcting the mischaracterization

`0532`'s own note called this "a systemd-notify-socket publisher" —
not what it is. Cross-checking against the real origin of this
command shape — containerd's own shim-v2 `publish`
(`~/git/containerd/cmd/containerd/command/publish.go:41-88`) — shows
what a *live* `publish` command actually does: reads a protobuf event
from stdin, dials containerd's own events gRPC service, and calls
`client.Publish(...)`. cri-o's own version copies the same `Name`/
`Usage: "receive shimv2 events"`/flag shape but strips all of that
real behavior out, leaving a bare `return nil`. This is genuinely
vestigial boilerplate inherited from the shimv2 command template, not
a systemd-notify integration — confirmed further by a repo-wide check
(`grep -rn "PublishCommand"` outside `vendor`/tests) showing nothing
in cri-o's own codebase ever invokes `crio publish` itself either.

## Faithful no-op, not a functional gap

Real crio's own `publish` subcommand does nothing observable itself
— there is no real behavior to replicate, only a real CLI-acceptance
gap to close (`ocicri publish ...` was previously a hard "unrecognized
subcommand" failure, where real `crio publish ...` exits `0`
silently). The same class as `0542`'s `Wipe --force` and `0564`'s
`ocibox create --verbose`.

## Why this is narrow and safe

Touches only `bin/ocicri/src/main.rs`'s own `Command` enum and its
one dispatch `match` arm — no other file, no shared crate, no other
command's behavior, no cgroup/namespace/capability/systemd/mount code
anywhere near it. No new persisted state, no per-lifecycle threading.

## Implementation

`Command::Publish { topic: Option<String>, namespace: Option<String>
}` — both flags accepted and immediately discarded at the dispatch
site (`Some(Command::Publish { topic: _, namespace: _ }) =>
return Ok(())`), the same shape `Command::Wipe { force: bool }`'s own
dispatch already established. Unlike real crio (which hides both the
subcommand and its flags from `--help`), this project keeps both
visible and thoroughly documented — matching this project's own
established "never hide a real, working flag" convention
(`oci_cli_common::args::GlobalArgs::debug`, `0561`).

## Tests

Three new integration tests in `tests/tests/ocicri_publish.rs`:
`publish_with_no_flags_is_a_silent_success`,
`publish_accepts_topic_and_namespace_flags_and_still_no_ops`, and
`no_subcommand_still_starts_the_real_server` (the same regression
check `ocicri_version_cli.rs`'s own identical test already
establishes for `version`/`wipe`, confirming this addition doesn't
disturb the default server-starting behavior).

Manually verified end to end beyond the automated tests: `ocicri
publish`, `ocicri publish --topic ... --namespace ...`, both exiting
`0` with no output; `ocicri --help`/`ocicri publish --help` both show
the new subcommand and its flags, fully documented.

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (129
test-result blocks, all passing — one new test file added, so the
block count is up from `0564`'s `128`; `RUST_TEST_THREADS=2` given
this host's own heavy, persistent concurrent-session CPU contention
this same day), `python3 ci/guards.py` (clean), `cargo deny check`
(clean), `bash ci/native-ci.sh` (clean on the first attempt), `bash
ci/build-deb.sh` (clean on the first attempt, real `dpkg -i`/
`--version`/`dpkg -r` round trip). A pure CLI-parsing-and-discard
addition — no hot path touched, no `ci/bench.sh` rerun needed.

## Deliberately still out of scope

Real crio's own remaining subcommands (`check`/`config`/`status`)
remain real, separate, much bigger gaps: `check` is a standalone
healthcheck-config CLI, `config` a config-file generator/validator,
and `status` a runtime status dump — each its own future increment,
not folded in here.
