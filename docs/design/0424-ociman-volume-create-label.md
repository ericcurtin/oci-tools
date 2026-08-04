# Design note 0424: `ociman volume create --label`

Status: implemented
Scope: `bin/ociman/src/volume.rs`, `bin/ociman/src/main.rs`,
`tests/tests/ociman_volume.rs`, `README.md`.

## What this closes

`ociman volume create` had no `--label` support at all, and this
project's own `VolumeRecord` had no labels field to store one in —
a real schema gap an earlier design note (`0423`) already flagged as
the blocking prerequisite for a future `volume ls --filter label=`.
This closes both: a real `labels` field now exists and is set-able,
readable back via `volume inspect`/`ls --format`.

## Real, checked-directly confirmation

`~/git/podman/cmd/podman/volumes/create.go:54`: `flags.
StringArrayVarP(&opts.Label, "label", "l", []string{}, ...)`, parsed
via `parse.GetAllLabels` (`~/git/podman/cmd/podman/parse/net.go:92-
98`): `key, value, _ := strings.Cut(label, "=")` — a bare `key` with
no `=` at all becomes `key=""`, never a parse error (only an empty
*key* is). This project already has the identical real grammar,
already reused by `ociman build --label`/`run --label`
(`build::parse_key_value_pairs`, confirmed to have the exact same
bare-key-means-empty-value behavior via its own existing unit test)
— reused verbatim here rather than writing a second, separate parser
for the same real rule.

`--driver`/`-d` and `--opt`/`-o` (real podman's other two `volume
create` flags, `create.go:50`/`57`) are deliberately **not**
implemented: this project has exactly one fixed "local" driver with
no options concept at all, so either flag would be a pure no-op with
nothing real to attach to — a real, honest narrowing, not an
oversight.

## Implementation

- `VolumeRecord` gains `#[serde(default)] labels: BTreeMap<String,
  String>` — the `#[serde(default)]` means a `metadata.json` written
  by an older binary (with no `labels` key at all) still reads back
  as a real, empty map rather than a hard parse error, verified with
  a dedicated test that hand-writes an old-shape JSON file directly.
- `VolumeStore::get_or_create` is now a thin wrapper over a new
  `get_or_create_with_labels(name, labels)`, which records `labels`
  only on real, first-time creation — **deliberately left untouched**
  on an already-existing volume (checked directly: real podman's own
  `--ignore`-tolerated re-`create` never overwrites a pre-existing
  volume's own already-recorded labels either), verified with a
  dedicated test giving different labels on a second, `--ignore`d
  call.
- `Command::VolumeCommand::Create` gains `label: Vec<String>`
  (`#[arg(long = "label", short = 'l')]`); `cmd_volume_create` parses
  it via `build::parse_key_value_pairs` and passes the result to
  `get_or_create_with_labels`.
- `VolumeView` (the shared `--json`/`--format` rendering struct
  `ociman volume create`/`ls`/`inspect` all already share) gains a
  `labels` field, always present (a real, honest `{}` when empty),
  matching this project's own already-established "always present,
  honestly empty" annotation convention.

## Tests

Three new unit tests in `bin/ociman/src/volume.rs`
(`get_or_create_with_labels_records_the_given_labels_on_first_
creation`, `get_or_create_with_labels_leaves_an_already_existing_
volumes_own_labels_untouched`, `a_metadata_json_with_no_labels_
field_at_all_reads_back_as_a_real_empty_map`) plus three new
integration tests in `tests/tests/ociman_volume.rs`
(`volume_create_label_records_every_given_label` — including the
bare-word-means-empty-value case, `volume_create_with_no_label_
reports_an_empty_labels_object`, `volume_create_ignore_on_an_
existing_volume_leaves_its_labels_untouched`). All prior tests in
both files continue to pass unmodified (194/194 in the `ociman`
unit-test binary, 40/40 prior + 3 new = 43/43 in `ociman_volume.rs`).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean, no diff), `cargo clippy --workspace --all-targets --locked --
-D warnings`, `cargo test --workspace --locked` (119 test-result
blocks, 0 failures, clean on the first full run), `python3 ci/
guards.py`, `cargo deny check`, `bash ci/native-ci.sh` (clean,
119/119), `bash ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg
-r` round trip). Touches only volume metadata/creation, not any hot
path at all — no benchmark re-run needed.

## Deliberately still out of scope

`ociman volume ls --filter label=`/`label!=` — this increment lands
the schema prerequisite (a real, per-volume `labels` field) but
doesn't itself add the filter key to `volume ls`; a natural, small,
separate follow-up now that the data actually exists to filter on,
reusing the exact same `LabelFilter`/`try_parse_label_filter`
primitives already shared everywhere else in this codebase.
`--driver`/`--opt` (see above — no real target exists for either).
