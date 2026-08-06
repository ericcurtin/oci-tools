# Design note 0519: `ociman image mount`/`ociman image unmount`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_image_mount.rs`.

## What this closes

A real, separate, non-alias pair of `ImageCommand` subcommands, not
implemented at all before this note. Three earlier design notes
(`0481`, `0482`, `0499`) had each mischaracterized `image mount`/
`unmount` as "cross-concept aliasing" of the already-existing
container `mount`/`unmount` (`0361`/`0511`) without actually checking
this source directly -- corrected here, transparently, not silently.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/images/mount.go`: `mountCommand`
  (`Parent: imageCmd`) calls `registry.ImageEngine().Mount` -- a
  genuinely different ABI method from `registry.ContainerEngine().
  ContainerMount`, with its own distinct `ImageMountOptions` type.
  With exactly one image given, no `--format`, no `--all`: prints
  just `reports[0].Path`.
- `~/git/podman/cmd/podman/images/unmount.go`: `unmountCommand`
  (`Aliases: []string{"umount"}`) calls `registry.ImageEngine().
  Unmount`; on success, prints each report's own `r.Id`.
- `~/git/podman/pkg/domain/infra/abi/images.go:158-230`
  (`ImageEngine.Mount`): with an explicit image list, resolves each
  (`ListImagesByNames`) then calls `i.Mount(ctx, nil, "")` for real --
  actually mounting (or reusing an already-mounted image's own real
  mount, incrementing a reference count).

## Implementation

New `ImageCommand::Mount { images: Vec<String> }` / `Unmount { images:
Vec<String> }` (the latter with `#[command(alias = "umount")]`,
matching real podman's own nested alias). Both resolve every given
image first (`resolve_by_reference_or_id`, `0122` -- a tag reference
or a real/short image ID fallback, matching real podman's own
`IMAGE-NAME-OR-ID` accepted input shape exactly), aborting the whole
call before mounting/printing anything if any one fails to resolve --
the same two-phase "resolve everything first" convention `container
unmount`'s own multi-target case (`0471`) already established.

`cmd_image_mount` then calls the exact same `oci_store::ensure_cached`
cache `ociman run`/`ociboot`/`ocibox create` already share (`0109`/
`0200`) for each resolved image, printing the returned cache
directory path -- an image already cached returns its own existing
path immediately (matching real podman's own "already mounted, just
increments a refcount" case in spirit); one never previously
extracted is built fresh, the identical real, measurable extraction
work `ociman run`'s own first use of any given image always pays
regardless.

`cmd_image_unmount` is a real no-op, printing each resolved image's
own 12-hex-char short ID (this project's own already-established
`images -q`/table convention for "the id" everywhere else, not real
podman's own literal full-length `r.Id`): this project's own rootfs
cache is permanent and content-addressed, never torn down by any real
reference count at all (only `ociman prune`'s own separate GC pass,
`0106`, ever actually removes a cache entry, once nothing references
it anymore) -- the identical reasoning `container unmount`'s own
`0361` no-op already established, applied to images instead of
containers.

**Deliberately narrower first slice** (matching this project's own
established "narrow first slice, document the rest" pattern, e.g.
`0361`'s own original container-mount scope before `--all`/bare-
mode/multi-id followed in `0470`-`0472`): no `--all`, no bare-
invocation "list every currently-cached image" mode, no `--format`.
Bare-mode listing in particular needs a real cache-digest-to-image-
reference reverse lookup this project has no existing primitive for
at all, unlike the container version (whose own bare-mode listing,
`0470`, only ever needed the already-existing container store's own
forward `id -> rootfs` mapping).

## Tests

Nine new integration tests in `tests/tests/ociman_image_mount.rs`:
resolution by reference and by short ID, multiple images in one call,
a resolution failure among several mounting nothing at all, unknown-
image/no-image error cases for both commands, the real no-op's
printed short ID, and the nested `umount` alias itself.

Full workspace: `cargo build --workspace --locked` (clean), `cargo
fmt --all` (clean), `cargo clippy --workspace --all-targets --locked
-- -D warnings` (clean), `cargo test --workspace --locked` (123
test-result blocks, one new test file added -- clean on the first
attempt with `RUST_TEST_THREADS=2`), `python3 ci/guards.py` (clean),
`cargo deny check` (clean). `native-ci.sh`: hit the documented
transient `ocicri_container.rs` flakiness twice under unusually heavy
host contention this turn (a second, genuinely concurrent process
observed via `ps aux`/load average, unrelated to this repo -- `git
fetch`/`git status` confirmed no actual concurrent repo modification
throughout), confirmed transient each time by rerunning the specific
failing test in isolation (passed both times), then a fully clean run
with `RUST_TEST_THREADS=1` throughout. `build-deb.sh` clean on the
first attempt (real `dpkg -i`/`--version`/`dpkg -r` round trip). A
new command reusing an already-hot-path-adjacent primitive
(`ensure_cached`) but not itself part of any hot path `ci/bench.sh`
measures -- no rerun needed.

## Deliberately still out of scope

`--all`/bare-mode listing/`--format` (see above). `ocibox upgrade`/
`export --app` (still flagged as ahead in `ocibox`'s own module-level
doc comment, genuinely bigger) remain open candidates for future
increments.
</content>
