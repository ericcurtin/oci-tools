# Design note 0476: `ocibox create --clone`/`-c`

Status: implemented
Scope: `bin/ocibox/src/main.rs`, `tests/tests/ocibox_create.rs`.

## What this closes

`ocibox create` had no `--clone` flag at all: an already-existing
box's own current state could never be used as the starting point for
a new one, matching real `distrobox create --clone`.

## Real, checked-directly confirmation

- `~/git/distrobox/internal/cli/create.go:82-88`: `--clone`/`-c`
  (`cli.StringFlag`, no default), doc string: *"name of the distrobox
  container to use as base for a new container ... useful to either
  rename an existing distrobox or have multiple copies of the same
  environment."*
- `~/git/distrobox/pkg/commands/create.go:114-119` (`Execute`) /
  `274-290` (`clone`): real distrobox refuses to clone a *running*
  container (`"cannot clone running container"`), then `podman
  commit`s the source into a brand-new image tag
  (`<lowercase-name>:<date>`), and finally runs an *ordinary* `create`
  using that commit tag as `containerImage` — i.e. the clone gets a
  real, full copy of the source's own *current* filesystem state
  (image layers plus every write since), not just a re-extraction of
  whatever the source's own original image looked like.
- Lines 195/225 (`makeContainerImage`/`makeContainerName`): `--clone`
  and `--image` are not strictly mutually exclusive in real
  distrobox's own code (`--clone`, when given, simply overwrites
  whatever `containerImage` `--image` or the default would have
  resolved to) — this project's own port chooses a stricter, clearer
  real error instead of a silent override when both are given, the
  same "an explicit error over an upstream's own more permissive
  silent override" preference this project has made before.
- `~/git/distrobox/internal/cli/ephemeral.go:60,85`: real `distrobox
  ephemeral` *does* inherit `--clone` too (its own comment: *"inherited
  create flags (e.g. -c/--clone)"*) — a real, deliberately deferred
  gap for this increment (see below).

## Why this project skips the image round-trip entirely

Real distrobox's own `commit`-then-`create` approach exists because
its underlying container engine (podman/docker) has a genuine, real
image store distinct from any one container's own writable layer —
`commit` is the only way to snapshot a running container's current
state into something a fresh `create` can start from. `ocibox`'s own
boxes have no such distinction at all: a box already *is* just a
plain `rootfs/` directory plus a `box.json` sidecar, with no
separate, read-only base image layer of its own to diff against. The
honest, correct equivalent for this simpler model is a direct
recursive copy of the source box's own current `rootfs/` — not a
`commit`-shaped round trip through `oci_store`'s own image blob
storage, which would be genuine, unneeded extra machinery for a
model that never had a container-vs-image distinction to begin with.

Real distrobox's own "cannot clone a *running* container" guard also
has no equivalent here at all: a box has no live, backgrounded
process to ever be "running" independently of an active `ocibox
enter` call (`docs/design/0207`) — cloning is always safe.

## Implementation

- `Command::Create::image`: `String` → `Option<String>`; new
  `#[arg(long = "clone", short = 'c')] clone: Option<String>`.
  Exactly one of the two must be given (a real, immediate error
  otherwise, in both directions).
- `create_box` split into two real paths behind the existing
  mutual-exclusivity check: `create_box_from_image` (the original
  resolve/pull/extract logic, unchanged) and the new `clone_box`
  (loads the source's own `box.json`, recursively copies its
  `rootfs/`, and builds a new `BoxRecord` carrying the source's own
  `image`/`env`/`working_dir` forward unchanged — there is no CLI
  override for any of those three at `create` time at all, cloned or
  not).
- New `copy_dir_recursive`: a small, dependency-free `cp -a`
  equivalent (directories, regular files with their own permission
  bits preserved, and symlinks via `read_link`/`symlink`) — needed
  since shelling out to `cp` is not one of this project's own allowed
  shell-outs (`ci/guards.py`), and no `walkdir`/`fs_extra`-shaped
  crate was already a dependency anywhere in the workspace.
- `--hostname`/`--home`/`--volume` remain fully independent of the
  clone source, exactly like an ordinary `--image` create: given
  explicitly, they override; left unset, their own already-
  established defaults apply (never inherited from the source box).

## Tests

Four new integration tests in `tests/tests/ocibox_create.rs`: a real
clone copies the source's own *current* rootfs (including a write
made after the source's own creation, proving this is a genuine
current-state copy, not a re-extraction of the original image) and
its `image`/`env`/`working_dir`, and is a genuinely independent copy
(a later write to the clone's own rootfs never reaches the source's);
cloning an unknown source box is a clear error, leaving no half-
created box directory behind; `--image`+`--clone` together, or
neither at all, are both clear errors; an explicit `--hostname`/
`--home` at clone time still applies independently of the source. All
12 tests in the file pass (8 prior + 4 new).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (121 test-result
blocks, 0 failures on the first attempt), `python3 ci/guards.py`
(clean), `cargo deny check` (clean), `bash ci/native-ci.sh` (hit
repeated transient, already-documented flaky failures across
`ociman_run.rs`'s own cgroup test and several `ocicri_container.rs`
tests on attempts 1-6 — this session's host was under unusually heavy
concurrent load, a second, unrelated `opencode` agent process
independently competing for CPU alongside the already-documented
long-running CPU-spinning background process; every single failure
was independently confirmed passing instantly in isolation, and the
full `ocicri_container.rs` file was separately confirmed passing
clean end to end with `--test-threads=2` before continuing to retry
the exact CI script unmodified — attempt 7 finally passed clean,
121/121), `bash ci/build-deb.sh` (clean, real `dpkg -i`/`--version`/
`dpkg -r` round trip on the first attempt). No benchmark re-run
needed: `ocibox create` is not exercised by `ci/bench.sh` at all.

## Deliberately still out of scope

- `ocibox ephemeral --clone` (real distrobox's own `ephemeral`
  inherits every `create` flag, including `--clone`) — this
  increment only reaches `create` itself; `cmd_ephemeral`'s own call
  site still always passes `None` for `clone`, documented inline.
- Real distrobox's own "cannot clone a running container" behavior is
  not replicated (see above: no honest equivalent exists in this
  project's own model at all — cloning is unconditionally safe here).
