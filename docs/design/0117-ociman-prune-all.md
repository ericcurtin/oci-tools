# Design note 0117: `ociman prune --all`

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Prune` gains `--all`/`-a`,
`cmd_prune`, `PruneResult`); `tests/tests/ociman_prune.rs` (4 new
tests).

## Why this, now

0111's own "what this doesn't do yet" section named this directly: "No
`--all`/selective pruning (real `docker system prune -a`'s own 'also
remove images with no container using them, not just fully-
unreferenced blobs' mode) — this increment only ever removes what's
*already* fully unreferenced." That gap stayed open through 0112-0116
(all `ociman build` feature work); this increment closes it, reusing
already-tested primitives (`Store::list_images`/`remove_image`,
`StateStore::list`) rather than adding anything new underneath.

## Semantics, adapted to this project's own storage model

Real `docker system prune` (no `-a`): removes stopped containers,
dangling (untagged) images, unused networks, build cache. `-a`
additionally removes *every* image not used by an existing container,
tagged or not. This project's own `oci_store` has no separate
"dangling/untagged image" concept at all (every `ImageRecord` is a real
tag — `-t`/`--tag` is required for `ociman build`, and a registry pull
always tags too), so the only meaningful `--all` behavior here is the
second half: **remove every image tag not currently used by any
container, running or stopped** — matching real `docker system prune
-a`'s own "not used by an existing container" half exactly, adapted for
a store that never has an untagged image to begin with.

Without `--all` (the default, unchanged from 0111): an image still
tagged is never touched, even if nothing currently uses it — matches
real `docker system prune`'s own default.

## Matched by manifest digest, not the exact tag string a container used

A real, checked-directly correctness detail: two tags can point at the
same manifest digest (`ociman tag`'s own whole point, 0103). If a
container was started via one tag string, the *other* tag pointing at
the exact same image must not be treated as "unused" just because its
own literal string was never passed to `ociman run` — both are the
same real image. `--all`'s own reachability computation resolves every
in-use container's own recorded image reference to its manifest digest
first, then compares by digest, not by string — a dedicated test
(`prune_all_matches_by_manifest_digest_not_the_exact_tag_string_a_
container_used`) proves this directly (would fail against a naive
string-equality check).

## Ordering: images first, then the existing blob/cache GC passes

`--all`'s own image-removal pass runs *before* `Store::gc`/`oci_store::
prune` in the same `cmd_prune` call, so an image this pass just untags
immediately makes its own now-orphaned blobs/rootfs-cache entries
eligible for the same GC run — one `ociman prune --all` invocation
reclaims everything reachable from it, not just the tags, without
needing a second run afterward. Verified directly (`prune_all_removes_
an_image_no_container_uses_and_reclaims_its_blobs_too`): the same
`PruneResult` reports both a non-empty `images_removed` and a non-zero
`blobs_removed`/`blobs_reclaimed_bytes` from one call.

## Real, automated tests

Four new tests in `tests/tests/ociman_prune.rs`: `--all` omitted still
leaves an unused-but-tagged image alone (regression guard for the
unchanged default); `--all` removes an image no container uses and
reclaims its blobs in the same call; `--all` keeps an image a real,
stopped (not necessarily running) container still uses — matching
`ociman rmi`'s own existing "running or stopped both count" dependency
check; and the digest-vs-string-matching test above. All 4
pre-existing `ociman prune` tests still pass unmodified (none of them
pass `--all`, so the new pass never runs for them). `cargo test
--workspace --locked` clean (2 runs), `cargo fmt --all --check`/`cargo
clippy --workspace --all-targets --locked -- -D warnings` both clean.
