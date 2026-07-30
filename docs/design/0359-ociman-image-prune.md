# Design note 0359: `ociman image prune`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_image_prune.rs`,
`README.md`.

## What this closes

`ImageCommand` (`ociman image ...`) had exactly one subcommand since
`0287`: `exists`. Real `podman image prune` (a distinct, narrower
sibling of `podman system prune`, `~/git/podman/cmd/podman/images/
prune.go`) had no equivalent here at all — a parallel gap to `0357`'s
own `ContainerCommand::Prune` (which itself was a distinct, narrower
sibling of `ociman prune`'s own container-reclaim pass, `0358`).

## Real, checked-directly semantics

Read `~/git/podman/cmd/podman/images/prune.go` directly: same
`--all`/`-a`, same `--filter` grammar as `podman system prune`'s own
image pass (delegates to the exact same `ImageEngine.Prune` real
podman itself uses for `system prune`'s own image step) — but this
command *never* touches a container, a pod, a volume, or a network at
all. `--build-cache`/`--external` (real podman's own build-container-
storage concepts, no equivalent here) are out of scope, same reasoning
`0357`'s own `--filter` deferral already used.

## Implementation

Factored `cmd_prune`'s own image-removal-plus-blob/cache-GC block into
a new shared `prune_images_and_reclaim(store, containers, all,
filters) -> ImagePruneOutcome` (`images_removed`/`blob_report`/
`cache_report`) — `cmd_prune` now calls it *after* pruning containers
(`0358`'s own established ordering, unchanged), and a new
`cmd_image_prune` calls it directly, with `containers` opened only to
read (`images_in_use_digests`), never pruned.

New `ImageCommand::Prune { all: bool, filter: Vec<String> }`;
dispatch wires it to `cmd_image_prune`. `--json` output is a new,
narrower `ImagePruneResult` (`images_removed`/`blobs_removed`/
`blobs_reclaimed_bytes`/`rootfs_cache_entries_removed`/
`rootfs_cache_reclaimed_bytes`) — deliberately omitting
`containers_removed`/`build_scratch_*`, both fields with no meaning
for this command at all (see `ImageCommand::Prune`'s own doc comment).

## Verified

New `tests/tests/ociman_image_prune.rs`:
`image_prune_on_an_empty_store_reports_nothing_to_reclaim` (also
asserts `containers_removed`/`build_scratch_entries_removed` are
*absent* from the JSON shape entirely, not merely empty);
`image_prune_without_all_leaves_an_unused_but_still_tagged_image_alone`;
`image_prune_all_removes_an_unused_tagged_image`;
`image_prune_never_removes_a_stopped_container` (even with `--all`,
proving the real, checked-directly scope difference from `ociman
prune` — the container survives, and because it does, its own image
stays correctly protected too, unlike `0358`'s own `ociman prune`
behavior on the identical setup);
`image_prune_filter_dangling_false_removes_a_tagged_image_without_all`
(the same `--filter` engine/override rule, reused unmodified).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures, full clean
run, no flakes), `python3 ci/guards.py`, `cargo deny check`, `bash
ci/native-ci.sh`, `bash ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).
