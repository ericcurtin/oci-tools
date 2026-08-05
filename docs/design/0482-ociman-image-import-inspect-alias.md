# Design note 0482: `ociman image import`/`image inspect` aliases

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_import.rs`,
`tests/tests/ociman_inspect.rs`.

## What this closes

Continuing the `ociman image` alias family `0478`-`0481` started:
`import` and `inspect` were still missing.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/images/import.go:25-44`: `imageImportCommand`
  (`Parent: imageCmd`) and `importCommand` (top-level) share `Use`/
  `Short`/`Long`/`RunE`/`Args`/`ValidArgsFunction` verbatim, plus
  `importFlags` registered on both — the identical pure-alias shape
  every other member of this family already has.
- `~/git/podman/cmd/podman/images/inspect.go:13-35`: `inspectCmd`
  (`Parent: imageCmd`) unconditionally sets `inspectOpts.Type =
  common.ImageType` before calling the exact same shared `inspect.
  Inspect` function real top-level `podman inspect --type image`
  (`~/git/podman/cmd/podman/inspect.go`) also reaches — never falling
  back to a container. Its own `init()` registers only `--format`/
  `-f` — no `--latest`/`--size`/`--type` at all, a narrower flag
  surface than the richer top-level `inspect`.

## Implementation

`import` is a pure dispatch-reuse addition, the exact same shape
every prior member of this family used: a new `ImageCommand::Import`
variant, field-for-field identical to the already-existing
`Command::Import`, dispatching into the same `cmd_import`.

`inspect` reuses an *existing* mechanism rather than needing anything
new: `Command::Inspect` already has an `inspect_type: InspectType`
field (`0409`) that, when forced to `InspectType::Image`, makes
`cmd_inspect` resolve image-only, never falling back to a container —
exactly real `podman image inspect`'s own behavior. `ImageCommand::
Inspect` is a thin wrapper exposing only `reference`/`format` (no
`--latest`/`--size`/`--type`, matching real `podman image inspect`'s
own narrower flag surface exactly), always passing
`InspectType::Image` to `cmd_inspect` — not a user-facing choice at
this level.

## Tests

Two new integration tests: `image_import_is_a_byte_identical_alias_
for_import` (`tests/tests/ociman_import.rs`), `image_inspect_is_a_
byte_identical_alias_for_inspect_type_image` (`tests/tests/
ociman_inspect.rs` — proves both the byte-identical output against
the already-established `--type image` flag *and* the real "never
resolves a container of the exact same name" behavior, reusing the
identical container/image name-collision fixture `inspect_type_
image_never_resolves_a_container_of_the_same_name` already
established). All 8 tests in `ociman_import.rs` pass (7 prior + 1
new); all 32 in `ociman_inspect.rs` pass (31 prior + 1 new).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (122 test-result
blocks, 0 failures — needed `--test-threads=2` on the fifth attempt;
this session's host was again under unusually heavy concurrent load
(the same already-documented long-running CPU-spinning background
process plus a second, independent `opencode` agent process), with
every individual flaky failure across the first four attempts
independently confirmed passing instantly in isolation before
retrying with reduced parallelism), `python3 ci/guards.py` (clean),
`cargo deny check` (clean), `bash ci/native-ci.sh` (clean, 122/122 on
the first attempt), `bash ci/build-deb.sh` (clean, real `dpkg -i`/
`--version`/`dpkg -r` round trip on the first attempt). No benchmark
re-run needed: neither `ociman image import` nor `image inspect` is
exercised by `ci/bench.sh`, and this is a pure dispatch-reuse addition
touching no existing function's body at all.

## Deliberately still out of scope

- `mount`/`unmount`/`diff` — the last three members of this real
  `podman image` family. Each is genuinely more involved than a pure
  alias: real `podman image mount`/`unmount` alias the *container*
  mount/unmount commands (a cross-concept aliasing shape, not yet
  independently verified in depth); real `podman image diff`
  computes a genuinely new "image vs. its own parent layer"
  comparison (`~/git/podman/cmd/podman/images/diff.go`, its own
  `diffRun`/`diff.Diff` with `DiffType` restricted to images) that
  this project has no equivalent logic for at all — `ociman diff`
  is container-only (matching real `podman container diff`'s own
  narrower scope, not the general top-level `podman diff` that
  auto-detects container-or-image the way `ociman inspect` already
  does), so there is no existing primitive to simply reuse here the
  way `inspect_type` already existed for `inspect`.
