# Design note 0520: `ociman image mount`/`unmount --all`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_image_mount.rs`.

## What this closes

`0519`'s own "Deliberately narrower first slice" section explicitly
flagged `--all` as still out of scope, alongside bare-mode listing
and `--format`. Re-examined directly this time: `--all` turns out to
need no new primitive at all (unlike bare-mode listing, which still
needs a real cache-digest-to-image-reference reverse lookup this
project has no existing primitive for) -- it's a pure forward sweep
over `Store::list_images`, already fully available.

## Real, checked-directly confirmation

- `~/git/podman/pkg/domain/infra/abi/images.go:194-198`
  (`ImageEngine.Mount`): `if opts.All { listImagesOptions.Filters =
  []string{"readonly=false"} }` -- `--all` sweeps every image via
  `ListImages`, filtered only by `readonly=false`, a condition every
  one of this project's own images already satisfies unconditionally
  (no read-only-image concept exists here at all).
- `~/git/podman/cmd/podman/images/mount.go`'s own `mount()`: `if
  len(args) > 0 && mountOpts.All { return errors.New("when using the
  --all switch, you may not pass any image names or IDs") }`.
- `~/git/podman/cmd/podman/images/unmount.go`'s own `unmount()`: the
  identical mutual-exclusivity check, plus `--force`/`-f`
  (`unmountFlags`).

**A real, checked-directly output-shape simplification found while
implementing this, worth documenting precisely**: real podman's own
`image mount` only prints a bare path (no id, no tab) when *exactly
one* image is given, no `--format`, no `--all` -- two or more images,
or `--all`, switches to a `{{.ID}}\t{{.Path}}` table even without
`--format` (checked directly, `mount.go`'s own bare-path branch is
gated on `len(args) == 1 && mountOpts.Format == "" && !mountOpts.All`
specifically). This is a real, checked-directly *difference* from the
**container** `mount` command (whose own bare-path branch, `0472`,
covers `--all`/`--latest`/multiple explicit containers alike, a
broader condition). `ociman image mount`, already implemented in
`0519` before this difference was noticed, always prints one bare
path per line regardless of image count -- this note keeps that
simpler, already-shipped convention for `--all` too rather than
introducing a second, template-based table code path just for this
one flag combination (matching `--format`'s own still-deferred
scope), and corrects `0519`'s own doc comment to document this real
divergence explicitly rather than leaving it implicit.

## Implementation

`ImageCommand::Mount`/`Unmount` gain `all: bool`; `Unmount` also
gains `force: bool` (accepted-and-ignored, the identical reasoning
[`Command::Unmount::force`] (`0361`) already establishes for
containers). A new shared `resolve_images_or_all` helper enforces the
exact mutual-exclusivity wording above, then either lists every
stored image (`--all`) or resolves the explicit list via `0519`'s own
`resolve_images_or_bail`. `--all` on an empty store succeeds silently
(no images to iterate), matching real podman's own identical
behavior.

## Tests

Six new integration tests in `tests/tests/ociman_image_mount.rs`:
`--all` mounting/unmounting every stored image, both commands' own
silent success on an empty store, the mutual-exclusivity error, and
`--unmount --force` accepted-and-ignored.

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (123
test-result blocks -- no new test file added, so the block count is
unchanged from `0519`; clean on the first attempt with
`RUST_TEST_THREADS=2`), `python3 ci/guards.py` (clean), `cargo deny
check` (clean), `bash ci/native-ci.sh` (clean on the first attempt
with `RUST_TEST_THREADS=2`), `bash ci/build-deb.sh` (clean on the
first attempt, real `dpkg -i`/`--version`/`dpkg -r` round trip). Not
part of any hot path `ci/bench.sh` measures -- no rerun needed.

## Deliberately still out of scope

Bare-invocation "list every currently-cached image" mode and
`--format` remain the only two gaps left from `0519`'s own original
scope statement -- both still need genuinely new machinery (a
reverse digest-to-reference lookup, and a real templated table
renderer respectively), not just wiring an already-existing
primitive.
</content>
