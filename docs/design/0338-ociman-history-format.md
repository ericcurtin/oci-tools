# Design note 0338: `ociman history --format`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_history.rs`,
`README.md`.

## What this closes

`0337`'s own "Still ahead" flagged `history --format`/`stats --format`
as real, small, immediate follow-ons reusing the exact same template
engine — real `podman history --format`/`docker history --format` are
real, documented, commonly-scripted flags, and `ociman history`
already builds one `Vec<HistoryEntryView>` with no engine changes
needed to reuse `render_format_template` against each row.

## Implementation

`Command::History` gained `format: Option<String>`. `cmd_history`
checks `format` first (before the existing `json`/table branches, and
after `views.reverse()` so the template sees the same newest-first
order the plain table/`--json` already use), rendering the template
against each `HistoryEntryView`'s own JSON value and printing one line
per entry — matching real `podman history --format`'s own identical
"one line per row" semantics, the same shape `ps`/`images`/`volume ls
--format` already established (as opposed to `inspect`/`info
--format`'s own single-object shape). Field names are `HistoryEntryView`'s
own JSON fields directly: `{{.created}}`, `{{.created_by}}`,
`{{.size}}`, `{{.comment}}`. `--format` takes priority over `--json`/
the default table when given; an unresolvable field path is a real,
immediate error — same precedence and error behavior the whole family
already established.

No new `#[allow(clippy::too_many_arguments)]` needed — `cmd_history`
only had two parameters before this, three now.

## Verified

`cargo build -p ociman --locked`; manual smoke test with a real loaded
image: `ociman history busybox:latest --format '{{.created_by}}'` and
`--format 'size={{.size}}'` both render correctly; `ociman history
--help` renders the new flag correctly.

Two new integration tests in `tests/tests/ociman_history.rs` (5 total,
3 pre-existing, all pass unchanged): a real two-entry history (a `RUN`
layer plus an `ENV` metadata-only entry, the same fixture the existing
JSON/table tests already establish) renders one line per entry in the
correct newest-first order; and `--format` taking priority over
`--json`/the default table plus a real, immediate error for an
unresolvable field, mirroring the whole `--format` family's own
identical coverage.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (113 test result blocks,
0 failures), `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).

Performance: `ociman history` is a one-shot, offline, read-only
command, not part of any hot-path benchmark tracked in
`docs/benchmarks.md`; the no-`--format` case is unchanged in shape and
cost (one extra branch check). No re-benchmark needed.

## Still ahead

`ociman stats --format` remains the one real, small, immediate
follow-on left in this family — deliberately not done in this same
note to verify each command's own real behavior individually first,
matching this whole family's own established pattern (`0332`-`0338`
have each landed one command at a time). `COPY --exclude=<pattern>`
(reusing this project's own already-threaded `DockerIgnore` filter
machinery, flagged in `0337`'s own survey) still needs its own
dedicated scoping pass before committing to it. `ociman`/`ocirun`'s
other remaining gaps (`--restart` policy, `--console-socket`) and
`ocibox`'s own remaining gaps (`stop`/`upgrade`/`generate-entry`/
`assemble`, `export --sudo`/`--enter-flags`) remain separately-scoped
future candidates.
