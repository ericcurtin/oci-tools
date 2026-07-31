# Design note 0383: `ocicri RemoveImage` refuses an in-use image

Status: implemented
Scope: `bin/ocicri/src/image_service.rs`, `tests/tests/
ocicri_container.rs`.

## What this closes

`ImageService::remove_image`'s own module doc comment (lines 37-43)
claimed real cri-o's `volumeInUse` in-use check "isn't ported here"
because "this project's own `ocicri` can't create any container via
CRI at all yet (every `RuntimeService` pod-sandbox/container RPC is
still a real, honest `Status::unimplemented`)." That claim is false
today — `CreateContainer`/`StartContainer`/`StopContainer`/
`RemoveContainer`/`ExecSync` have all been real since design notes
0236-0238, well before this doc comment was last touched — the same
kind of drift `0380`/`0382` already found and fixed elsewhere in this
codebase. The actual code never got the equivalent check: `remove_image`
happily deleted every reference sharing a manifest digest even while a
real, persisted `ContainerRecord` still pointed at that exact digest.

## Real, checked-directly confirmation

- `~/git/cri-o/server/image_remove.go`'s own `volumeInUse` (lines
  133-150): walks every container (any state, no filter) and returns a
  bare `fmt.Errorf("the image is in use by %s", container.ID())` if
  any references the digest — never wrapped in a `status.Error`, so it
  surfaces over real gRPC as `codes.Unknown`.
- `~/git/container-libs/storage/store.go`'s `DeleteImage` (lines
  2776-2786): the identical rule from the other direction — any
  container (unfiltered by state) referencing the image ID is a real,
  surfaced `ErrImageUsedByContainer`.
- `ociman rmi`'s own `rmi_one` (`bin/ociman/src/main.rs`) already
  replicates this correctly for its own container store — `ocicri` was
  the one caller of the shared `oci_store::remove_image` primitive
  that never got the equivalent guard.
- `ContainerRecord::image_ref` (`bin/ocicri/src/container.rs`) already
  stores the exact resolved manifest digest a container was created
  from (0237) — a direct string comparison against `resolved.record().
  manifest_digest.to_string()`, no re-resolution needed (simpler than
  `ociman`'s own equivalent check, whose container annotation only
  stores the raw image *name*, needing a second resolve step).
- **`RemoveImageRequest` has no force-equivalent field at all**
  (confirmed directly, `oci-cri-types/proto/api.proto:1799-1804`) —
  the fix is an unconditional refusal, not `ociman rmi --force`'s own
  cascading remove-the-containers-too option.
- A nuance worth being precise about: `oci_store::remove_image` only
  ever deletes the one reference-pointer JSON file (`crates/oci-store/
  src/images.rs`'s own `remove`, a plain `fs::remove_file`) — it never
  touches blob content directly. So the guard isn't preventing an
  *immediate* synchronous deletion of bytes a live container is
  reading from (a created container already has its own independent,
  fully-extracted rootfs copy, unaffected by the blob store either
  way). It prevents two real, still-meaningful problems instead: (a)
  CRI-visible state inconsistency — a live container's own
  `ContainerStatus.image_ref` would point at an image `ListImages`/
  `ImageStatus` can no longer see at all; and (b) a real, if indirect,
  data-loss path — once every tag pointer for a digest is gone, that
  digest is no longer "reachable" from any `ImageRecord`, so a later
  GC pass against the same shared storage root could actually reclaim
  the underlying blob bytes out from under a still-referenced,
  still-running container.

## Implementation

- `remove_image` computes the digest (unchanged), then — before the
  removal loop — loads every persisted `ContainerRecord`
  (`container::load_all`, already tolerant of a missing
  container-store directory, returning an empty list rather than an
  error) and collects the IDs of any whose `image_ref` matches the
  digest, in any `ContainerState`.
- If any dependents exist, returns `Err(Status::unknown(...))` — the
  message lists every dependent container id (this project's own
  established `ociman rmi`-style "list every dependent" wording,
  richer than real cri-o's own bare first-match single-container
  message, which isn't part of any documented proto contract worth
  matching verbatim) — before ever touching `store.remove_image`.
- `Status::unknown`, not `invalid_argument`/`failed_precondition`,
  matching the exact convention this project's own `CheckpointContainer`
  already established for "a bare, unwrapped real error surfaced as
  `codes.Unknown`" (`bin/ocicri/src/runtime_service.rs`).
- The stale module doc comment is rewritten to describe what's
  actually implemented now, citing the real cri-o/container-libs
  sources checked directly above.

## Tests

New integration test in `tests/tests/ocicri_container.rs`:
`remove_image_refuses_while_a_container_still_references_it` — a real
gRPC round trip over the actual built `ocicri` binary's Unix socket,
using a second, independent `ImageServiceClient` stub connected to the
exact same socket a `RuntimeServiceClient` already created a sandbox +
container against. Creates a container (deliberately left merely
`Created`, never started — the narrowest possible case, proving *any*
state blocks removal, not just running), attempts `RemoveImage`, and
asserts a real `tonic::Code::Unknown` naming the dependent container
id, with the image still fully present afterward (not partially
deleted). Then removes the container and confirms the identical
`RemoveImage` call now succeeds — a regression guard against the new
check being over-broad. All 28 tests in `ocicri_container.rs` (27
pre-existing + 1 new) pass, plus all 14 pre-existing tests in
`ocicri_image_service.rs` unmodified.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (0 failures), `python3
ci/guards.py`, `cargo deny check`, `bash ci/native-ci.sh`, `bash
ci/build-deb.sh` (real `dpkg -i`/`--version`/`dpkg -r` round trip).
This change is `ocicri`-only and touches no `ocirun`/`ociman` code
path at all — `ci/bench.sh` doesn't measure `ocicri` (confirmed by
grep, and by its own module doc comment's already-documented reason:
a long-lived gRPC server's serving latency, not process startup, is
what would matter, and this change is on neither) — no benchmark
re-verification needed.
