# Design note 0409: `ociman inspect --type`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_inspect.rs`,
`README.md`.

## What this closes

`ociman inspect` had no `--type`/`-t` at all: it always tried a
container first, falling back to an image if none resolved (this
project's own existing default). Real `podman inspect --type`/`-t`
lets a caller force resolution to exactly one kind — a container
sharing a name with an unrelated image (a real, if unusual,
possibility, since the two namespaces are genuinely independent) had
no way to disambiguate which one `ociman inspect` should actually
resolve.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/common/inspect.go`: `AllType = "all"`
  (the default), `ContainerType = "container"`, `ImageType =
  "image"` (plus `network`/`pod`/`volume`/`artifact`, none of which
  this project has an equivalent generic `inspect` for — each already
  has its own dedicated subcommand, e.g. `ociman volume inspect`, the
  same established convention this note doesn't change).
- `~/git/podman/cmd/podman/inspect/inspect.go`'s own `inspect`
  function: `case common.ContainerType:` calls straight into
  `containerEngine.ContainerInspect`, and `case common.ImageType:`
  calls straight into `imageEngine.Inspect` — neither ever falls back
  to the other kind on a miss. Only `case common.AllType:` (the
  default) does the "try container, then image" dance
  (`inspectAll`), matching this project's own pre-existing behavior
  exactly.

## Implementation

- A new `InspectType` (`clap::ValueEnum`, `All`/`Container`/`Image`,
  `All` the `#[default]`) mirrors the established `PsSortKey`/
  `SaveFormat` pattern already used elsewhere in this file.
- `Command::Inspect` gains `--type`/`-t` (`inspect_type:
  InspectType`, `default_value_t = InspectType::All`).
- `cmd_inspect`'s existing container-then-image resolution is
  restructured so `InspectType::Image` skips the container-resolution
  attempt entirely (going straight to the pre-existing image lookup,
  whose own "no such image" error is unchanged), and
  `InspectType::Container` — when the container attempt genuinely
  fails to resolve — now returns a real, immediate "no such
  container" error instead of silently falling through to the image
  branch. `InspectType::All` (the default) is completely unchanged
  from before this flag existed.

## Tests

Two new end-to-end integration tests in `tests/tests/ociman_inspect.rs`:
`inspect_type_image_never_resolves_a_container_of_the_same_name` (a
real container and a real image both exist; the default `--type all`
resolves the container, but `--type image` on that same name is a
real error, while `--type image` on the actual image reference still
succeeds) and `inspect_type_container_never_resolves_an_image` (an
image exists with no container of any matching name; `--type
container` on that image's own reference is a real "no such
container" error, not a silent fallback to the image). All existing
tests continue to pass unmodified (22/22 in `ociman_inspect.rs`).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures), `python3
ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh`, `bash
ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg -r` round trip).
This touches only `cmd_inspect`'s own resolution logic, not any hot
path at all — no benchmark re-run needed.

## Deliberately still out of scope

Real podman's own further `--type network`/`pod`/`volume`/`artifact`
values remain unimplemented — this project has no generic `network`/
`pod`/`artifact` concept at all, and `volume` already has its own
dedicated `ociman volume inspect` subcommand rather than being folded
into this one, matching this project's own already-established
convention.
