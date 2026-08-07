# Design note 0554: `ociman import --quiet`/`-q`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_import.rs`.

## What this closes

Adds `--quiet`/`-q` to `ociman import` and its `ociman image import`
alias — the one remaining sibling in the `pull`/`push`/`save`/`load`/
`import` family that had never been given the flag (`push --quiet`
closed the previous gap in this same family, `0548`).

## Real, checked-directly confirmation

- Flag definition, on both the top-level and nested command:
  `~/git/podman/cmd/podman/images/import.go:88` —
  `flags.BoolVarP(&importOpts.Quiet, "quiet", "q", false, "Suppress
  output")`.
- Real consumption: `~/git/podman/pkg/domain/infra/abi/images.go:
  524-526` — `if !options.Quiet { importOptions.Writer = os.Stderr }`
  — the exact same progress-writer-gating pattern already ported into
  this project for `pull`/`push`/`save`/`load --quiet`
  (`0417`/`0428`/`0548`).

## Real, functional gap — not a no-op

`cmd_import` already draws a real `indicatif` spinner
(`"importing"`), unconditionally, with no `quiet` parameter to gate
it — the exact same class of progress-writer this project's own
`pull`/`push`/`save`/`load --quiet` already correctly suppress via the
shared `progress::spinner_unless_quiet` helper. `import` was simply
the one command in this family never given the flag at all.

## Implementation

`bin/ociman/src/main.rs`: `quiet: bool` (`#[arg(short, long)]`) added
to `Command::Import` and `ImageCommand::Import`. `cmd_import` gained a
`quiet: bool` parameter (needing `#[allow(clippy::too_many_arguments)]`
now, matching the same allowance several other multi-flag commands in
this file already carry), and its one `progress::spinner(...)` call
site was swapped for `progress::spinner_unless_quiet(quiet, ...)` —
the identical, already-proven-safe pattern this exact helper already
serves four other call sites with (`pull`, `push`, `save`, `load`).

## Tests

`import_quiet_still_imports_correctly`
(`tests/tests/ociman_import.rs`): imports a real single-layer tar with
`--quiet`, confirms it succeeds and the resulting image actually runs
correctly — the same "accepted, still produces correct output" test
shape `ociman_save.rs`'s own `save_quiet_still_writes_a_correct_
archive` already established (the spinner itself draws only to
stderr, and is already automatically hidden whenever stderr isn't a
real terminal, same established limitation every other spinner-backed
command's own test already notes).

Manually exercised beyond the automated tests: `ociman import --help`/
`ociman image import --help` render the new flag correctly; a real
`ociman import`/`ociman import --quiet` of the same tar produced
byte-identical resulting images.

## Verification

`cargo build --workspace --locked` (clean), `cargo fmt --all` (clean,
no changes needed for the new test), `cargo clippy --workspace
--all-targets --locked -- -D warnings` (clean), targeted
`ociman_import.rs` run (9/9, 8 pre-existing + 1 new). Pure CLI-parsing
plus reuse of the already-tested `spinner_unless_quiet` helper — no
new hot path, no `ci/bench.sh` rerun needed.
