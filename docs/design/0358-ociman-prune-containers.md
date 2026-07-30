# Design note 0358: `ociman prune` also reclaims containers

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_prune.rs`,
`README.md`.

## What this closes

While implementing `0357`'s own `ociman container prune`, `ociman
prune`'s own doc comment claim — "matches real `docker system prune`/
`podman system prune`'s own default exactly" — turned out to itself be
a real, checked-directly *false* claim, the same kind of stale/wrong
citation `0351` already found and fixed once before (there, a prior
design note's own claim about `podman exec --preserve-fds`).

## Real, checked-directly semantics

`~/git/podman/cmd/podman/system/prune.go`'s own real `SystemPrune`
call sequence (`~/git/podman/pkg/domain/infra/abi/system.go`) removes
containers strictly *before* images, and does so **unconditionally**
regardless of `--all` at all (`--all` only widens the *image* pass to
untagged-or-not). Confirmed directly against a real installed `podman
system prune -f`, no `--all`:

```
$ podman run --name prune-test-container busybox true
$ podman system prune -f
Deleted Containers
0a58006c0588...
Total reclaimed space: 10.96kB
$ podman ps -a --filter name=prune-test-container   # gone
```

And with `--all`, the now-unused image is reclaimed in the very same
call, not a second one:

```
$ podman run --name prune-inuse busybox true
$ podman system prune -a -f
Deleted Containers
33a3389d...
Deleted Images
e0e8b3cbfed6...
```

This project's own pre-existing `ociman prune` never touched
containers at all — an image only a stopped container used stayed
"in use" forever until that container was separately `rm`'d.

## Implementation

Factored `0357`'s own `cmd_container_prune` removal loop out into a
shared `prune_eligible_containers(&StateStore) -> Vec<String>`
(`Created`/`Stopped` only, `remove_container(.., force: true, ..)` —
see its own doc comment for why `force: true` is always safe here),
now called from both `cmd_container_prune` and `cmd_prune`. `cmd_prune`
calls it **before** `images_in_use_digests`, matching real podman's
own checked-directly containers-before-images ordering exactly: a
container removed in this same pass no longer keeps its own image
artificially "in use" for the image-removal loop that follows.

`PruneResult` (`ociman prune --json`'s own shape) gained a new
`containers_removed: Vec<String>` field, populated unconditionally
(never gated by `--all`, matching containers' own unconditional real
scope); plain-text output gained a matching `containers: removed N
(...)` line, printed first (mirroring real podman's own container-
before-image print order too).

## Verified

Updated `tests/tests/ociman_prune.rs`: renamed and rewrote
`prune_all_keeps_an_image_a_stopped_container_still_uses` into
`prune_all_removes_a_stopped_containers_own_now_unused_image_too`
(the corrected, checked-directly expectation: both the container and
its own now-orphaned image are removed together); added a new
`prune_all_keeps_an_image_a_genuinely_running_container_still_uses`
(the one real "in use" case that still survives: a real, detached,
still-running container, never touched, its image correctly still
protected); updated
`prune_all_matches_by_manifest_digest_not_the_exact_tag_string_a_container_used`
to match (both aliased tags are now removed together once the one
container briefly using either is itself pruned first). All 24
pre-existing, unaffected `ociman_prune.rs` tests re-run unmodified and
still pass.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures, full clean
run), `python3 ci/guards.py`, `cargo deny check`, `bash
ci/native-ci.sh`, `bash ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).
