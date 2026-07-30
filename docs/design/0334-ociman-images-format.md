# Design note 0334: `ociman images --format`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_images.rs`,
`README.md`.

## What this closes

`0332`/`0333` flagged `images --format` as the last natural consumer of
the shared Go-template-lite engine — real users reach for `docker
images --format '{{.Repository}}:{{.Tag}}'` just as commonly as
`inspect`/`ps --format`. This closes that trio.

## Implementation

`Command::Images` gained `format: Option<String>` — no `-f` short
alias, the same real reason `ps --format` (`0333`) has none: `images`
already has its own `-f`/`--filter`. Reuses `0332`'s own
`render_format_template`/`resolve_json_path`/`format_json_scalar`
completely unchanged: `cmd_images` checks `format` first (before the
existing `quiet`/`json`/table branches), rendering the template against
each listed `ImageView`'s own JSON value and printing one line per
image — matching real `podman images --format`'s own identical "one
line per row" semantics. Field names are `ImageView`'s own JSON field
names directly (`{{.reference}}`, `{{.digest}}`, `{{.size}}`) — note
`{{.digest}}` prints the *full* `sha256:...` digest from the JSON
record, not the 12-hex-char short form `-q`/the table's own `DIGEST`
column compute via a separate, display-only `short_digest` closure; a
real, honest consequence of "field names are this project's own JSON
output directly," not an inconsistency to paper over. `cmd_images`
needed no new `#[allow(clippy::too_many_arguments)]` (unlike `ps`,
`0333`) — it only had two parameters before this, four now, well under
the threshold.

## Verified

`cargo build -p ociman --locked`; manual smoke test with a real,
loaded image: `ociman images --format '{{.reference}} {{.digest}}'`
and `--format 'size={{.size}}'` both render correctly; `ociman images
--help` renders the new flag correctly.

Two new integration tests in `tests/tests/ociman_images.rs` (12 total,
10 pre-existing, all pass unchanged): one line per listed image with
the correct field substitution; and `--format` taking priority over
`-q`/`--json`/the default table plus a real, immediate error for an
unresolvable field, mirroring `inspect`/`ps --format`'s own identical
coverage.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (113 test result blocks,
0 failures), `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).

Performance: `ociman images` is a one-shot, offline, read-only command,
not part of any hot-path benchmark tracked in `docs/benchmarks.md`; the
no-`--format` case is unchanged in shape and cost (one extra branch
check). No re-benchmark needed.

## Still ahead

The `inspect`/`ps`/`images --format` trio real users reach for most is
now complete, all three sharing one unchanged engine. `ociman`/
`ocirun`'s own other remaining gaps (`--restart` policy, `ocirun run
--no-pivot`/`--console-socket`) and `ocibox`'s own remaining gaps
(`stop`/`upgrade`/`generate-entry`/`assemble`, `export --sudo`/
`--enter-flags`) remain separately-scoped future candidates.
