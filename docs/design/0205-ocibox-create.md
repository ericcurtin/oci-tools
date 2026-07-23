# Design note 0205: `ocibox create`

Status: implemented
Scope: `bin/ocibox/Cargo.toml` (new `oci-store`/`oci-registry`/
`oci-layer`/`oci-spec-types`/`serde`/`serde_json` dependencies);
`bin/ocibox/src/main.rs` (`Command::Create`, `cmd_create`,
`extract_rootfs`, `validate_box_name`, `BoxRecord`, `boxes_root`);
`tests/tests/ocibox_create.rs`.

## Continuing milestone 7, after 0204's own preparatory groundwork

A periodic check-in this turn confirmed the project's own recent arc
(0200-0203 `ociboot`, 0204 a preparatory `oci_registry` refactor
explicitly framed around unblocking `ocibox`) was deliberate, not
drift — and that this turn was the natural point to actually deliver
`ocibox`'s first real subcommand, rather than deferring again.

Studied real `distrobox create` directly from its own actual
implementation (`~/git/distrobox`, the project's real Go rewrite, not
just its docs) before scoping anything: its real flag surface is large
(X11/Wayland/audio/nvidia passthrough, init-hooks, additional-package
installation, cloning an existing box). A full port needs its own
careful, multi-turn effort. Also investigated actually *launching* a
box (real namespaces/mounts, matching `ociman create`/`ocirun start`'s
own two-phase `oci_runtime_core::launch::create` + `exec_fifo::
signal_start` lifecycle) — that lifecycle turns out to be fully usable
as shared, already-tested library code with **no** need to duplicate
`ociman`'s own private detached-keeper-process machinery (that exists
specifically for `ociman run -d`'s own "start it running in the
background and confirm it actually started" case; `launch::create`
itself already "leaves the container's own init process running in
the background once this function returns... no extra double-fork/
daemonization step needed", per its own doc comment) — but assembling
a correct bundle (rootfs setup, spec synthesis, home-directory bind
mount, bundle directory layout) is still substantial enough that doing
it justice, plus `ocibox enter` to actually use it, is better scoped as
its own dedicated future increment rather than bolted onto this one.

This increment is deliberately just the first slice: resolving an
image and extracting a real, dedicated rootfs for a named box — the
same "narrow first, document the rest" pattern this project has used
successfully throughout (e.g. `ociboot build-image` before `ociboot`'s
own eventual `install to-disk`).

## The fix

```
ocibox create --image <REFERENCE> --name <NAME> [--pull]
```

* `--name` validated with the same conservative charset check
  `ociman run --name`/`ociman rename` already established (kept as its
  own small, deliberate local duplicate — four lines, not worth a new
  cross-binary dependency).
* Refuses a name already in use (a `<boxes_root>/<name>` directory
  already existing) — matching real `distrobox create`'s own identical
  refusal, rather than silently overwriting.
* Resolves/pulls the image via the now-shared `oci_registry::
  resolve_or_pull` (0204) — `PullPolicy::Missing` by default (pull only
  if not already present), `PullPolicy::Always` with `--pull`, matching
  real `distrobox create --pull`'s own flag exactly.
* Extracts every layer into a real, dedicated, writable rootfs
  directory (`oci_layer::apply`, one call per layer, bottom-first) —
  deliberately *not* through `oci_store`'s own shared, read-only
  `rootfs_cache`: that cache exists so many short-lived `ociman run`
  containers of the *same* image never each pay the extraction cost,
  but a pet container needs its own independent, writable copy for its
  entire (potentially very long) lifetime; sharing the cached
  extraction directly would let a write inside one box corrupt every
  other container of the same image, exactly the hazard `oci_store::
  rootfs_cache`'s own module doc comment already warns against.
* Persists a minimal `box.json` record (name, image, manifest digest,
  created timestamp) under `<boxes_root>/<name>/`, `boxes_root` a
  sibling of `oci_store`'s own `blobs`/`images` directories — this
  project's own established convention for per-capability state living
  directly under the one shared storage root (`containers/` for
  `ociman`, `rootfs-cache`/`build-scratch` for its own build cache,
  `boxes/` here), rather than a second, independent storage root.
* A failed extraction (or write of `box.json`) removes the
  half-created box directory before returning its own real error, so a
  later retry under the same name doesn't spuriously trip the
  already-exists check against a broken, partial directory.

## Verified by hand

* A real image resolves, pulls if needed, and extracts a real,
  complete rootfs (`ls .../rootfs/bin` shows real busybox applets, a
  real symlink included) — confirmed directly.
* Creating the same name twice is a clear, real error on the second
  attempt.
* An invalid name is rejected before any pull/extraction is even
  attempted.
* A reference that resolves to a real registry request but fails
  there (an unreachable host) leaves no box directory behind at all.

## Tests

Four new integration tests in `tests/tests/ocibox_create.rs`: a real
rootfs is extracted and a correct `box.json` persisted; a duplicate
name is refused; an invalid name is rejected (and never even creates
the `boxes` directory); a failed pull leaves no box directory behind.
Four new unit tests for `validate_box_name` directly. Full `cargo
build --workspace --locked`/`cargo test --workspace --locked` (2 clean
runs, 86/86 result blocks — one more than before, the new test
binary)/`cargo fmt --all --check`/`cargo clippy --workspace
--all-targets --locked -- -D warnings`/`python3 ci/guards.py`/`cargo
deny check`/`bash ci/native-ci.sh` all clean. No performance
regression (`ociman run --rm`, ~63ms, consistent with prior
measurements — this change adds an entirely new binary's own code,
touching nothing on `ociman`/`ocirun`'s own call path; `ocibox create`
itself, for an already-pulled small image, completes in ~16ms).

## What this doesn't do yet

Actually launching a box (`ocibox enter`, the real namespace/mount/
home-directory-bind-mount setup via `oci_runtime_core::launch::
create`/`exec_fifo::signal_start`), `ocibox list`/`rm`/`stop`,
X11/Wayland/audio/nvidia passthrough, init-hooks, additional-package
installation, and cloning an existing box are all still ahead — this
increment is genuinely just "resolve an image and give a named box its
own real, dedicated rootfs," nothing more.
