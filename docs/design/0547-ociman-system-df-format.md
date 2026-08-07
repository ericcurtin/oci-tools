# Design note 0547: `ociman system df --format`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_system_df.rs`.

## What this closes

`docs/design/0263`'s own first cut explicitly deferred `--format`, and
`docs/design/0285` (`-v`/`--verbose`) restated it as "still ahead" in
its own closing section, unrevisited since. This closes it for the
summary (non-verbose) shape.

## Real, checked-directly confirmation

- Flag definition: `~/git/podman/cmd/podman/system/df.go:47-49` —
  `flags.StringVar(&dfOptions.Format, "format", "", "Pretty-print
  images using a Go template")`.
- Real, checked mutual exclusivity: `df.go:59` — `if dfOptions.Format
  != "" && dfOptions.Verbose { return errors.New("cannot combine
  --format and --verbose flags") }`.
- Real consumption, and the exact row shape: `df.go:88-146`
  (`printSummary`) builds a `[]*dfSummary` — one row per `Type`
  (`"Images"`/`"Containers"`/`"Local Volumes"`) with `Total`/`Active`/
  `RawSize`/`RawReclaimable` (`Size()`/`Reclaimable()` methods,
  `df.go:266-274`) — and the *default*, no-`--format` case
  (`df.go:141-146`) already renders that identical shape via
  `{{range . }}{{.Type}}\t{{.Total}}\t{{.Active}}\t{{.Size}}\t
  {{.Reclaimable}}\n{{end -}}`: `--format` is genuinely just letting
  the caller substitute their own per-row template for that same
  already-established row-per-`Type` shape, not a fundamentally
  different rendering path.

## Real, functional gap (not a no-op)

This project already has a full, repeatedly-used Go-template-*lite*
engine (`render_format_template`) and the `SystemDfRow` structs
`cmd_system_df` already computes — this closes real, missing wiring,
not an inapplicable concept. Before this, `--format` was simply not a
defined flag on `SystemCommand::Df` at all.

## Implementation

`bin/ociman/src/main.rs`: `format: Option<String>`
(`#[arg(long = "format", value_name = "TEMPLATE")]`) added to
`SystemCommand::Df`. `cmd_system_df` gained a `format: Option<&str>`
parameter, checked first against `verbose` (real podman's own exact
`"cannot combine --format and --verbose flags"` wording, ported
verbatim — this project deliberately doesn't invent a verbose-shape
template of its own, matching the "only the summary shape, matching
real podman's own identical restriction" scope this increment set out
with).

A new `SystemDfFormatRow` struct (`{{.type}}`/`{{.total}}`/
`{{.active}}`/`{{.size_bytes}}`/`{{.reclaimable_bytes}}`) is a
deliberately separate shape from the existing `SystemDfView` (the
flat, three-named-fields object `--json` already uses) — matching
real podman's own per-`Type`-row `[]*dfSummary` list instead, with
this project's own already-established field-naming convention
(lowercase `snake_case`, not real podman's own capitalized `Type`/
`Total`/`Active`/`Size`/`Reclaimable`) — the identical "this project's
own JSON field names, not real podman's own capitalized struct field
names" convention `stats --format`'s own doc comment (`0545`) already
established. One `render_format_template` call per row, printed one
line per row — matching this project's own already-established "no
`{{range}}` needed in the user's own template" convention every other
multi-row `--format` (`ps`/`images`/`volume ls`/`history`/`stats`)
already uses, a deliberate, already-precedented narrowing of real
podman's own richer Go-template semantics (where the user's own
`--format` string there is expected to supply its own `{{range}}` if
per-row output is wanted).

## Tests

Four new tests in `tests/tests/ociman_system_df.rs`:

- `df_format_renders_one_line_per_row_with_this_projects_own_field_
  names` — an empty store, all three rows rendered with all-zero
  fields.
- `df_format_reports_a_real_images_own_size` — a real seeded image,
  confirming the `Images` row's own `total` reflects it.
- `df_format_and_verbose_together_is_a_clear_error` — the real,
  checked-directly mutual-exclusivity wording.
- `df_format_with_an_unknown_field_is_a_clear_error` — matching every
  other `--format`-enabled command's own identical convention.

Manually exercised beyond the automated tests: `ociman system df
--format '{{.type}}: total={{.total}} active={{.active}}
size={{.size_bytes}} reclaimable={{.reclaimable_bytes}}'` against a
real pulled image, `--format ... --verbose` (real error),
`--format '{{.bogus}}'` (real error), and `--help` rendering the new
flag's own doc comment correctly.

## Verification

`cargo build --workspace --locked` (clean), `cargo fmt --all` (clean,
no changes needed for the new tests), `cargo clippy --workspace
--all-targets --locked -- -D warnings` (clean), targeted
`ociman_system_df.rs` run (15/15, 4 new + 11 pre-existing), a full
`cargo test --workspace --locked` run (clean), `python3 ci/guards.py`
(clean), `cargo deny check` (clean), `bash ci/native-ci.sh` (clean),
`bash ci/build-deb.sh` (clean, real `dpkg -i`/`--version`/`dpkg -r`
round trip). Pure CLI-parsing plus reuse of the already-tested
`render_format_template` engine — no new hot path, no `ci/bench.sh`
rerun needed.
