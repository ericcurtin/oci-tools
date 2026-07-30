# Design note 0368: `ociman diff --format`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_diff.rs`,
`README.md`.

## What this closes

`0149`'s own `ociman diff` never got a `--format` flag at all, unlike
`ps`/`images`/`inspect`/`history`/`volume ls`/`info` (`0332`-`0339`),
which all already route through the shared Go-template-*lite* engine.
Real `podman diff`/`docker diff --format` do have one.

## Real, checked-directly semantics — genuinely narrower than the other `--format` commands

Read `~/git/podman/cmd/podman/diff/diff.go` directly, expecting
symmetry with `ps`/`images`/`inspect`'s own rich engine — found a real
divergence instead: `diff`'s own `--format` is *not* a Go-template
engine at all. `report.IsJSON(options.Format)` recognizes only
`json` (or the template forms `{{json}}`/`{{json .}}`, not modeled
here — narrowed for simplicity, see below); anything else is a real,
immediate error: `"only supported value for '--format' is 'json'"`,
reused verbatim. The flag's own help text confirms this too:
`"Change the output format (json)"` — not the usual `"...--format
{{.field}}"` phrasing every other `--format`-capable command here
uses.

`--format json` and this project's own global `--json` flag produce
identical output; when both are given, `--format` wins outright —
matching real podman's own identical per-command-flag-over-global
precedence (checked directly: `diff.Diff` only ever consults
`options.Format`, never falling back to any other flag).

## A deliberate narrowing

Only the plain literal `json` is accepted — real podman's own
`{{json}}`/`{{json .}}` alternate template spellings for the same
JSON request are not modeled here at all, a small, deliberate
simplification: they're a rarely-used escape hatch in real podman's
own implementation (letting a literal Go template reach the `json`
keyword unambiguously), not a meaningfully different code path from
the plain `json` case already covered.

## Implementation

New `Command::Diff::format: Option<String>`. `cmd_diff` resolves the
effective JSON-or-table decision once, up front: `Some("json")` ->
`true`; `Some(_other)` -> a real, immediate `anyhow::bail!` with real
podman's own exact error text; `None` -> falls back to the existing
global `--json` flag, completely unchanged. No new rendering logic at
all — the exact same `if json { ... } else { ... }` branch `0149`
already had.

## Verified

New tests in `tests/tests/ociman_diff.rs`:
`diff_format_json_matches_the_global_json_flags_own_output`;
`diff_format_json_wins_over_a_conflicting_global_json_false`;
`diff_format_rejects_anything_other_than_json` (a real Go-template-
looking value, `{{.added}}`, to also confirm this command's own
`--format` genuinely isn't the rich engine the other `--format`
commands use). All 5 pre-existing `ociman_diff.rs` tests re-run
unmodified and still pass.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures, full clean
run, no flakes), `python3 ci/guards.py`, `cargo deny check`, `bash
ci/native-ci.sh`, `bash ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).
