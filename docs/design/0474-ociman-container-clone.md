# Design note 0474: `ociman container clone` (first slice)

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_clone.rs`.

## What this closes

`ociman` had no `container clone` subcommand at all. Real `podman
container clone` copies an existing container's effective config into
a brand-new one. This lands the first, deliberately narrower slice:
cloning always from the exact same image the source used (no
positional `IMAGE` override yet), `--destroy`, `--force`, `--run`.

## Real, checked-directly confirmation

- `~/git/podman/cmd/podman/containers/clone.go` (whole file):
  `Use: "clone [options] CONTAINER NAME IMAGE"`, `Args:
  cobra.RangeArgs(1, 3)`; `--destroy`, `--run`, `--force`/`-f` flags;
  `--force` without `--destroy` is `"cannot set --force without
  --destroy"` (verbatim, reused here); prints only the new
  container's id (`fmt.Println(rep.Id)`).
- `~/git/podman/pkg/domain/infra/abi/containers.go:1766-1868`
  (`ContainerClone`): builds the new spec directly from the *existing*
  container's own current, already-resolved config
  (`generate.ConfigToSpec`) rather than re-deriving it from CLI flags
  — a deliberate choice this project's own port matches for a real
  reason (see below), not just convenience. `--destroy`/`--run` both
  happen *inside* this function, in that order, *before* the id is
  ever returned/printed.
- `~/git/podman/pkg/specgen/generate/container.go:606-640`
  (`CheckName`): the exact real default-name collision-avoidance
  algorithm — `<name>-clone`, then `<name>-clone1`, `<name>-clone2`,
  ... for the first free one — ported here verbatim.

## Why this project clones the spec directly, not via CLI-flag re-derivation

Real podman's own `ConfigToSpec` approach copies the *existing*
container's fully-resolved runtime config verbatim onto a new
specgen, only afterward letting `FillOutSpecGen` apply any *new*,
clone-time-only overrides on top. This project's own equivalent —
literally cloning the source's own on-disk `config.json` — is not
just simpler, it is the only *correct* option available: this
project's persisted state has no way to tell an image-provided
environment variable apart from an explicit `--env` override after
the fact (both just end up as entries in the same final `process.env`
list), so reconstructing a `RunArgs` and re-running `prepare_container`
would be a real, silently-lossy re-derivation. Cloning the spec
directly needs no such reconstruction at all.

## Implementation

- New `ContainerCommand::Clone { container, name, destroy, force, run
  }` (podman's own `clone` is `container`-only, no bare top-level
  `ociman clone` alias — checked directly, real podman has none
  either).
- `cmd_clone`:
  1. Resolves the source (`resolve_container_id`), loads its
     `PersistedState` and its bundle's `config.json`
     (`oci_runtime_core::Bundle::load`).
  2. Reads the source's own `ANNOTATION_IMAGE`, resolves that same
     image via `store.resolve_image` and its manifest.
  3. Computes the new name: given explicitly (validated, checked for
     a collision), or `<source-name-or-id>-clone`/`-cloneN` via the
     `CheckName`-equivalent loop above.
  4. `create_container_record` (the exact same shared primitive
     `prepare_container` itself uses) allocates a fresh id/bundle
     directory.
  5. Extracts a **fresh, independent** rootfs from the same image's
     layers (`oci_layer::apply` per layer, the plain `Extract` path
     only — a real, honest first-slice scope narrowing versus this
     project's own separate rootless-overlay-rootfs optimization, not
     a correctness gap) — never a copy of the source's own current,
     possibly-modified rootfs, matching real podman's own identical
     "genuinely new container, storage-wise" semantics.
  6. Clones `source_bundle.spec` verbatim, adjusts only `root.path` to
     the new rootfs directory, writes it as the new bundle's
     `config.json`, then round-trips it through the same `Bundle::
     load`+`validate::validate` every other bundle-writing call site
     already uses.
  7. Finalizes the new record to `Status::Created` (`create_
     container_record`'s own placeholder default is `Creating`) — the
     exact same terminal state `cmd_create` itself always leaves a
     brand-new container in; clone never launches anything itself
     unless `--run` says otherwise.
  8. `--destroy`: reuses `remove_container` directly (the same
     low-level primitive `cmd_rm` itself calls) — a running source
     still needs `--force`, matching this project's own already-
     established `rm` rule exactly.
  9. `--run`: reuses `cmd_start(&new_id, false)` directly (detached,
     never attached).

## A real bug found and fixed while wiring this up

`cmd_start`'s own reused path already prints the new container's id
itself once it confirms the clone actually started
(`launch_detached_and_confirm`'s own `print_id` parameter, `0186`).
This command's own first draft printed the id a *second* time
unconditionally at the end regardless of `--run` — a real double-
print bug, caught by this increment's own `clone_run_starts_the_
clone_detached` test (which asserts exactly one output line) before
landing. Fixed by only printing directly when `--run` was *not* given
— matching real podman's own single `fmt.Println(rep.Id)` call
exactly, unconditional on `--run` there too (since `ctr.Start` is a
pure Go API call with no CLI output of its own).

## Tests

Nine new integration tests in `tests/tests/ociman_clone.rs`: basic
clone creates a real, separate `Created` container with an
independent rootfs and the default `<name>-clone` name; the default-
name collision-avoidance algorithm (`-clone`, then `-clone1`); an
explicit `NAME` positional; an explicit name collision is a clear
error; `--destroy` removes a genuinely *stopped* source (a merely
`created`, never-started source still needs `--force` too, matching
`ociman rm`'s own already-established rule — a real, deliberately
corrected assumption in this test's own first draft); `--force`
without `--destroy` is a clear error; `--destroy` on a running source
needs `--force`; `--run` starts the clone detached (the double-print
regression test above); cloning an unknown container is a clear
error. All 9 pass.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all`
(clean), `cargo clippy --workspace --all-targets --locked -- -D
warnings` (clean), `cargo test --workspace --locked` (121 test-result
blocks — one more than the prior 120, this increment's own new test
file — 0 failures on the second attempt; the first attempt hit three
transient, already-documented flaky failures in `ocicri_container.rs`,
confirmed unrelated and passing instantly in isolation), `python3
ci/guards.py` (clean), `cargo deny check` (clean), `bash
ci/native-ci.sh` (one transient, already-documented flaky failure on
the first attempt, same file, clean 121/121 on the second), `bash
ci/build-deb.sh` (clean, real `dpkg -i`/`--version`/`dpkg -r` round
trip on the first attempt). No benchmark re-run needed: this change is
a pure addition (225 insertions, 0 deletions in `bin/ociman/src/
main.rs`) that never modifies any existing hot-path function's body,
and `ociman container clone` is not exercised by `ci/bench.sh` at all.

## Deliberately still out of scope

- A positional `IMAGE` argument (cloning onto a genuinely *different*
  image than the source used) — real podman's own richer 3-arg form;
  needs its own pull/resolve-and-merge-with-overrides logic.
- Every real `create`-time resource/health/etc. override flag real
  `podman container clone` also accepts on top of the source's own
  config (`--cpus`, `--memory`, `--pod`, ...) — this first slice
  clones the source's config unmodified only.
- The rootless-overlay-rootfs fast path for the clone's own new
  rootfs (always the plain `Extract` path here) — a real, honest
  scope narrowing, not a correctness gap.
