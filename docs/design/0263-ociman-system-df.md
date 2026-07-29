# Design note 0263: `ociman system df`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_system_df.rs`.

## A real, commonly-used podman command with no equivalent yet

`podman system df` (real disk usage across images, containers, and
local volumes) had no `ociman` equivalent at all — a real, commonly
used command (unlike `podman generate systemd`, checked directly and
found `[DEPRECATED]` in a real installed podman, or `podman search`,
which needs a Docker-Hub-specific v1 search API this project's own
established "spin up a real local registry" test methodology can't
exercise). This slice adds `ociman system df` (no `-v`/`--format` yet
— the default summary table only), checked directly against real
podman's own `pkg/domain/infra/abi/system.go`/`cmd/podman/system/
df.go`.

## Composition, with one real, honest simplification

Every number this needs was already computable from existing,
already-tested primitives — `Store::image_summary` (`ociman images`'
own size column), the `in_use_digests` set `ociman prune` already
builds, `oci_store::dir_size` (the same hardlink-aware walk `ociman
prune`'s own rootfs-cache reporting already uses), and
`containers_using_volume` (`ociman volume rm`/`prune`'s own real
"is anything still using this" check).

- **Images**: deduplicated by manifest digest (two tags of the same
  real image count once, matching real podman's own dedup-by-
  `ImageID`); `active` = referenced by ≥1 container; `reclaimable` =
  the size of every wholly *unused* image. Real podman's own formula
  is more precise — `ImagesSize` minus the summed *unique*
  (non-cross-image-shared) size of only the in-use images, so a
  shared-but-unused layer of an in-use image still counts as
  reclaimable there. This project has no per-image "unique vs. shared"
  size breakdown anywhere yet, so this reports the simpler "total size
  of wholly unused images" instead: never an overcount, but a real,
  narrower undercount whenever an unused image happens to share layers
  with an in-use one — a materially bigger feature (a real digest-
  reference-count pass across every stored image) to close exactly,
  deliberately deferred and documented honestly rather than guessed at.
- **Containers**: `size`/`reclaimable` come from each container's own
  real writable-layer directory. A real bug caught before this ever
  shipped: a container using this project's own rootless-overlay
  optimization (`docs/design/0108`-`0110`) leaves its own `rootfs/`
  directory genuinely *empty* on disk once stopped (the overlay mount
  is what populates it, only while the container's mount namespace is
  alive) — the real, persisted writable delta lives in a separate
  `upper/` directory instead, the same one `resolve_container_root`
  already checks for the identical reason. An initial implementation
  using `bundle/rootfs` unconditionally reported `0B` for every
  overlay-backed container regardless of what it had actually written
  — found by a hands-on manual smoke test (a real container writing a
  real 100KB file, then `system df` still showing `0B`), not by
  inspection, and fixed before any test was even written by checking
  for `upper/`'s own presence first, falling back to `rootfs/` for a
  plain-`Extract` container where the writable content really does
  live there directly.
- **Local Volumes**: exact `total`/`size`; `active`/`reclaimable`
  follow `containers_using_volume`'s own already-established rule.

## Verified

Integration (`tests/tests/ociman_system_df.rs`):

- An empty store reports all-zero, both as JSON and in the plain-text
  table (real header/row labels present).
- A real, wholly unused image is 100% reclaimable.
- Two tags of the same real image are deduplicated to one.
- After a real `ociman run` that writes a known 64KiB file: the image
  becomes `active` (no longer reclaimable); the now-stopped
  container's own writable-layer size is real and correctly reflects
  the write (catching the overlay-vs-extract bug directly, since this
  development host's own rootless-overlay optimization is active by
  default) and is itself fully reclaimable (not running).
- A freshly created, empty, unreferenced volume reports zero size and
  is not `active`.

Full workspace: `cargo build`/`test --workspace` (109 test binaries),
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets --
-D warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh`.

## Still ahead

`-v`/`--verbose` (the real per-image/per-container/per-volume
breakdown table) and `--format` are still ahead. The precise
per-image "unique vs. shared across other stored images" size
breakdown (closing the one documented simplification above) would need
a real digest-reference-count pass across every stored image.
