# Design note 0402: `ociman run/create --label-file`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `bin/ociman/src/build.rs`,
`tests/tests/ociman_inspect.rs`, `README.md`.

## What this closes

Real `podman run --label-file`/`podman create --label-file` — reading
additional `KEY=value`/bare-`KEY` label entries from a file, the exact
same real-world convenience `--env-file` already provides for
environment variables — had no equivalent anywhere in this project.
`--label` itself already existed (`0274`); this closes the one
remaining sibling flag.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/common/create.go`'s own
  `labelFileFlagName` — a plain, repeatable `--label-file`.
- `~/git/podman/cmd/podman/parse/net.go`'s own `GetAllLabels`: real
  podman shares one real parsing function, `parseEnvOrLabelFile`,
  between its own `--env-file` and `--label-file` — the identical file
  format (trim leading whitespace, skip a now-empty or `#`-starting
  line) this project's own `read_env_file` (`0341`) already
  implements. Confirms the two flags are, upstream, genuinely "one
  format, two callers," not a coincidence.

## Implementation

- `RunArgs` (shared by `Command::Run`/`Command::Create` via
  `#[command(flatten)]`) gains `label_file: Vec<PathBuf>`, doc-commented
  the same way `env_file` already is: repeatable, later files override
  earlier ones for a shared key, `--label` always wins regardless of
  flag order.
- `build::read_label_file`, a thin, distinctly-named wrapper around
  the already-existing `read_env_file` — matching real podman's own
  "one format, two callers" shape exactly rather than duplicating the
  same trim/skip logic under a second name.
- `prepare_container`'s existing label-building loop (image's own
  inherited `Config.Labels`, then `--label`'s own pairs merged on top)
  gains one more step in between: each `--label-file`'s own entries
  (via `build::parse_key_value_pairs`, the same function `--label`
  itself already uses) fold in first, in the order given on the
  command line, before `--label` is applied last — the identical
  precedence shape `combined_env`'s own `--env-file`-then-`--env`
  construction already established.

## Tests

Two new unit tests for `read_label_file` (blank/comment-line
skipping, a missing path is a real error) — the identical fixture
`read_env_file`'s own existing tests use, read back through the new
name. Two new end-to-end integration tests in
`tests/tests/ociman_inspect.rs` (which already hosts every other
`--label` test): `create_label_file_reads_entries_from_a_real_file_
and_merges_with_the_image` (a real file's entries merged with the
image's own inherited labels, verified via `ociman inspect`'s own
`labels` field) and `create_label_flag_always_wins_over_label_file_
regardless_of_order` (the same fixed-precedence proof `--env-file`'s
own equivalent test already established, `--label` given first on the
command line but still winning). All existing tests continue to pass
unmodified.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures), `python3
ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh`, `bash
ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg -r` round trip).
This touches `prepare_container`'s own label-building loop, not
`launch.rs`'s hot path at all — an empty `label_file` (the common
case) costs one no-op loop over an empty `Vec`, the same negligible
shape `0341`'s own `--env-file` addition already established (that
note needed no `ci/bench.sh` re-run either, for the identical reason).

## Deliberately still out of scope

Real podman's own `--label-file` has no documented special values or
additional CLI-level validation beyond the shared file format itself
— nothing else to port. Every other unmodeled `ociman run`/`create`
flag surveyed alongside this one (`--device`, host-device passthrough)
remains a real, separate, unrelated gap, matching this project's own
already-documented scope limits.
