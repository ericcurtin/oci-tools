# Design note 0563: `ociman version --format`/`-f`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_version.rs`.

## What this closes

`docs/design/0162` (the note that originally implemented `ociman
version`) explicitly named `--format <go-template>` under "what this
doesn't do yet," reasoning at the time that no other command in this
project's CLI surface had a Go-template engine either. That rationale
is now stale: `0332` onward built exactly that engine for
`inspect`/`ps`/`images`/`volume ls`/`info`/`history`/`stats`/`system
df`/`commit`/`diff` — `version` was the one command left behind by
its own immediate neighbor `info`. No later note (checked through
`0562`) ever revisited or closed this. This adds it.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/system/version.go:39`: flag registration —
  `flags.StringVarP(&versionFormat, formatFlagName, "f", "", "Change
  the output format to JSON or a Go template")`.
- `~/git/podman/cmd/podman/system/version.go:51-90` (`PrintVersion`):
  real, live consumption — a real Go-template render when given,
  matching every other `--format`-using command's own established
  precedence in this project (`--format` wins over `--json`/the
  default plain-text report).

## Real functional gap, not a faithful no-op

Unlike `0562`'s `--process-label`/`--apparmor` (where the underlying
subsystem genuinely doesn't exist), this project's own Go-template-
lite engine is real and fully functional — adding `--format` to
`version` is a genuine, immediately-working feature, not an honest-
rejection stub. `ociman version --format '{{.git_commit}}'` now
produces real, correct output.

## Why this is narrow and safe

Pure CLI parsing plus formatting — no kernel-level namespace/cgroup/
capability/systemd/mount interaction of any kind. `cmd_version`
builds a `VersionReport` (already `#[derive(Serialize)]`) from
`env!("CARGO_PKG_VERSION")`, a build-time git hash constant, and
`Platform::host()` — no container, no store, nothing kernel-facing.
The change reuses the already-existing, already-tested
`render_format_template` helper every other `--format` command in
this project already shares, following the exact same "format wins
over `--json`" precedence `cmd_info` already established one variant
above `Command::Version` in the same enum.

## Implementation

`Command::Version` changes from a bare unit variant to `Version {
format: Option<String> }` (`#[arg(short, long = "format", value_name
= "TEMPLATE")]` — a short `-f` alias, matching real podman's own
identical registration exactly; no collision within this one-field
variant). `cmd_version` gains a `format: Option<&str>` parameter and
an `if let Some(template) = format` branch identical in shape to
`cmd_info`'s own, placed before the existing `--json` check.

## Tests

Three new integration tests in `tests/tests/ociman_version.rs`:
`version_format_renders_multiple_fields_and_takes_priority_over_json`,
`version_format_short_alias_behaves_identically`, and
`version_format_of_an_unknown_field_is_a_clear_error` — mirroring
`ociman_info.rs`'s own identically-shaped tests for the same engine.

Manually verified end to end beyond the automated tests: `ociman
version --format`/`-f` with single and multiple placeholders, an
unknown-field error, and `--format` correctly taking priority over
`--json` when both are given.

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (128
test-result blocks, all passing — no new test file added, so the
block count is unchanged from `0562`; `RUST_TEST_THREADS=2` given
this host's own heavy, persistent concurrent-session CPU contention
this same day), `python3 ci/guards.py` (clean), `cargo deny check`
(clean), `bash ci/native-ci.sh` (clean on the first attempt), `bash
ci/build-deb.sh` (clean on the first attempt, real `dpkg -i`/
`--version`/`dpkg -r` round trip). A pure CLI-parsing-and-formatting
addition — no hot path touched, no `ci/bench.sh` rerun needed.

## Deliberately still out of scope

Real podman's own `version --format` also supports rendering the
`Server:` half via a template (`versionFormat = strings.ReplaceAll
(versionFormat, ".Server.", ".")`), a real, checked-directly no-op
here: this project has no daemon/server component of its own at all,
matching the same "no `Server:` section" reasoning `Command::
Version`'s own pre-existing doc comment already gives.
