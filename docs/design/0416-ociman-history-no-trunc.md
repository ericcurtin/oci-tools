# Design note 0416: `ociman history --no-trunc`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_history.rs`,
`README.md`.

## What this closes

`ociman history`'s plain table has always unconditionally truncated
a long `CREATED BY` entry at 60 characters plus `...` (real, long
shell commands from a Dockerfile `RUN` are the common case this
matters for), with no way to see the full command without switching
to `--format`/`--json`. Real `podman history --no-trunc` toggles
exactly this. `ociman ps --no-trunc` already established the
identical pattern (same table-truncation concept, same flag name) —
this closes the same real gap for `history`.

## Real, checked-directly confirmation

`~/git/podman/cmd/podman/images/history.go`:
- Flag registration (`historyFlags`, line ~72): `flags.BoolVar(&opts.
  noTrunc, "no-trunc", false, "Do not truncate the output")`.
- `historyReporter.CreatedBy()` (lines 143-146): `if !opts.noTrunc &&
  len(...) > 45 { ... + "..." }` — real podman's own threshold is 45,
  not 60; this project's own 60-char threshold predates this change
  (an earlier, independent choice already baked into the existing
  plain-table printer, `0104`) and is left untouched here — this
  increment only ever toggles *whether* truncation happens, not the
  threshold itself, matching the flag's own real scope exactly.

## Implementation

- `Command::History` gains `#[arg(long = "no-trunc")] no_trunc: bool`
  alongside the existing `format` field.
- `cmd_history` gains a `no_trunc: bool` parameter; the plain-table
  loop's existing truncation condition becomes `if no_trunc ||
  view.created_by.chars().count() <= 60`, mirroring `ps --no-trunc`'s
  own `display_command` closure exactly. `--format`/`--json` are
  untouched — both already print the full, untruncated string either
  way, matching real podman's own `--no-trunc` having no effect on
  either.

## Tests

New test in `tests/tests/ociman_history.rs`,
`history_no_trunc_shows_the_full_command_only_in_the_plain_table`: a
real build with a `RUN` command deliberately longer than 60
characters; asserts the plain table truncates (contains `...`, not
the full string) without the flag, shows the full string with no
`...` with the flag, and that `--json` already showed the full string
either way. All 5 prior `ociman_history.rs` tests continue to pass
unmodified (6/6 total).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean, no diff), `cargo clippy --workspace --all-targets --locked --
-D warnings`, `cargo test --workspace --locked` (119 test-result
blocks, 0 failures on a clean run — one earlier attempt hit the
known, pre-existing `ocicri_container.rs` host-contention flake,
confirmed environmental via an immediate isolated rerun),
`python3 ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh`
(clean, 119/119), `bash ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip). Touches only `ociman history`'s own
plain-table printer, not any hot path at all — no benchmark re-run
needed.

## Deliberately still out of scope

Real podman's own `history --human`/`--quiet` flags are not
implemented — `--human` only ever changes the already-human-readable
`SIZE` column's own formatting (a real, separate, smaller cosmetic
gap), and `--quiet` (image-ID-only output) has no obvious target
here since this project's `history` output has no per-row image ID
field to begin with (every row is a layer/metadata entry within the
*same* image, unlike `podman images -q`).
