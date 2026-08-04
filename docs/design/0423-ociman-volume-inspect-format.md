# Design note 0423: `ociman volume inspect --format`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_volume.rs`,
`README.md`.

## What this closes

`ociman volume inspect` had no `--format`/`-f` support at all —
`--json` or the identical pretty-JSON default were the only two
output shapes. Real `podman volume inspect --format` renders one or
more fields via a Go-template string. This closes that gap, reusing
the exact same rendering engine and precedence rule already shared
by `ociman inspect`/`ps`/`images`/`volume ls --format` (`0332`-
`0335`).

## Real, checked-directly confirmation

`~/git/podman/cmd/podman/volumes/inspect.go:41-43`: `flags.
StringVarP(&inspectOpts.Format, "format", "f", "json", ...)`, with
its own example `podman volume inspect --format "{{.Driver}}
{{.Scope}}" myvol` — the identical `--format`/`-f` shape this
project's own `Command::Inspect`/`ps`/`images`/`volume ls --format`
already established, confirming this genuinely is the same real
mechanism, not a coincidence of naming.

## Implementation

This is, deliberately, the smallest possible version of this
increment: the rendering machinery already existed and was already
in use by the sibling `ociman inspect` command in the very same
file, for the very same JSON-object shape (`VolumeView`, a plain
`#[derive(Serialize)]` struct — no special handling needed).

- `VolumeCommand::Inspect` gains `format: Option<String>`
  (`#[arg(long = "format", short = 'f', value_name = "TEMPLATE")]`).
- `cmd_volume_inspect` gains a `format: Option<&str>` parameter; its
  entire manual `if json { print_json } else { to_string_pretty }`
  body is replaced with a single call to the already-existing shared
  `print_inspect_result(&view, json, format)` — the exact same
  helper `cmd_inspect`'s own container/image branches already call,
  confirmed to produce byte-identical pretty-JSON output to what the
  old manual body did (`json_string`'s own `serde_json::to_string_
  pretty` is exactly what the removed manual `else` branch already
  called directly).

## Tests

Two new tests in `tests/tests/ociman_volume.rs`, mirroring `volume
ls --format`'s own existing sibling tests exactly:
`volume_inspect_format_renders_the_requested_fields` (renders
`{{.name}}={{.driver}}`, and confirms `-f` behaves identically to
`--format`) and `volume_inspect_format_takes_priority_over_json_and_
errors_on_an_unknown_field` (both precedence and error-message
checks). All 38 prior tests in `ociman_volume.rs` continue to pass
unmodified (40/40 total).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean, no diff), `cargo clippy --workspace --all-targets --locked --
-D warnings`, `cargo test --workspace --locked` (119 test-result
blocks, 0 failures, clean on the first full run), `python3 ci/
guards.py`, `cargo deny check`, `bash ci/native-ci.sh` (clean,
119/119), `bash ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg
-r` round trip). Touches only `ociman volume inspect`'s own output
rendering, not any hot path at all — no benchmark re-run needed.

## Deliberately still out of scope

`ociman volume create --label`/`--driver`/`--opt` remain
unimplemented — `--label` is a real, small, separate gap (this
project's `VolumeRecord` has no labels field yet at all — the same
schema prerequisite an earlier design note already flagged as
blocking `volume ls --filter label=`), while `--driver`/`--opt` would
be pure no-op flags: this project has exactly one fixed "local"
driver with no options concept at all, so accepting either would
have nothing real to attach to yet.
