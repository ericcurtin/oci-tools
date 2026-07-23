# Design note 0213: `ocicri` `ImageService` — `ListImages`/`ImageStatus`

Status: implemented (first real slice; `PullImage`/`RemoveImage`/
`ImageFsInfo`/`StreamImages` deliberately still ahead)
Scope: new `crates/oci-store/src/resolve.rs` (moved from `ociman`-
private code, zero behavior change); `bin/ociman/src/main.rs` (now
calls the shared version); `bin/ocicri/src/image_service.rs`;
`bin/ocicri/src/main.rs` (registers `ImageService` alongside
`RuntimeService`); `tests/tests/ocicri_image_service.rs`.

## Continuing 0212's own first slice

0212 gave `ocicri` a real, running gRPC server with `RuntimeService.
Version` answered. This increment adds `ImageService`'s own two
read-only RPCs — `ListImages`/`ImageStatus` — reusing this project's
own already-tested `oci_store` primitives directly rather than
building anything new: `ociman images`/`ociman inspect` already do
almost exactly this work.

## Shared prerequisite: image resolution moved into `oci_store`

CRI's `ImageSpec.image` field is routinely a bare digest/ID (it must
equal `PullImageResponse.image_ref`/`Image.id`, never necessarily a
tag), so `ocicri` needed the exact same "resolve by tag, or fall back
to a real/short image ID" logic `ociman`'s own private
`resolve_image_by_reference_or_id`/`resolve_image_by_id_only`
(0122/0179) already had — moved into `crates/oci-store/src/resolve.rs`
as `resolve_by_reference_or_id`/`resolve_by_id_only`/`ResolvedImage`/
`untagged_reference`/`is_untagged_reference`, matching this project's
own "share as much code as possible" pillar and the identical
extraction pattern already used for `oci_registry::resolve_or_pull`
(0204). A new `StoreError::AmbiguousId` variant carries the one error
case the old code raised via `anyhow::bail!` (same exact message text,
`"image ID {spec:?} is ambiguous: matches {count} different images"`).
`ociman`'s own call sites are unchanged (`use oci_store::{... as
resolve_image_by_reference_or_id, ...}` aliases preserve every original
local name) — verified as a genuine, zero-behavior-change move: every
one of `ociman`'s own existing tests exercising this logic (`rmi`,
`inspect`, `tag`, `push`, `prune`) passes completely unmodified against
the shared version, plus seven new unit tests directly in `oci_store`
itself (including the ambiguous-ID case, which `ociman`'s own test
suite never actually exercised before).

## Real semantics, checked directly against real `cri-o`

Both RPCs' edge-case behavior was checked against real `cri-o`'s own
source (`~/git/cri-o/server/image_list.go`/`image_status.go`), not
guessed from the proto's prose alone:

* `ListImages`'s own `filter` field, when given a non-empty image
  spec, resolves *just that one image* (0 or 1 results) rather than
  filtering a larger list — real `cri-o`'s own code comment says so
  explicitly ("kubelet never uses the filter... fall back to existing
  code instead of having an extra code path"). A filter matching
  nothing is a real, empty list, never an error.
* `ImageStatus` of an unresolvable image returns a real, empty response
  (`image: None`), never a gRPC error — only a request naming *no*
  image at all is a real `InvalidArgument` error.
* `Image.uid`/`Image.username`: real `cri-o`'s own `getUserFromImage`
  logic ported exactly — split on `:` first (a `user:group` form only
  ever looks at the user half), then try parsing as a numeric uid,
  else treat as a username.
* `ImageStatusResponse.info`'s own verbose-only single `"info"` key
  holding a JSON blob of `{labels, imageSpec}` — the exact same shape
  real `cri-o`'s own `createImageInfo` produces.
* `Image.repo_digests`: one `<registry>/<repository>@<digest>` entry
  per distinct repository among an image's own real tags — matching
  real `cri-o`'s own fallback for a storage backend with no separately
  -tracked digest references (`ConvertImage`'s own `PreviousName + "@"
  + Digest` case — this project's own store has the identical shape:
  no separate repo-digest tracking, only real, resolvable tags).

## Verified by hand

A real image seeded into the store (matching this project's own
established fully-offline, no-registry-access test fixture
convention): `ListImages` reports it with a real `sha256:...` id, real
tags, and a nonzero size (straight from the manifest's own descriptor
sizes, no filesystem stat needed); a filter naming one of two
genuinely different seeded images (different content, hence different
manifest digests) returns only that one; `ImageStatus` resolves both
by exact tag and by a short prefix of the same real image ID (proving
the shared `oci_store::resolve_by_reference_or_id` primitive
genuinely backs this RPC, not just exact-tag lookups); verbose
`ImageStatus` reports real labels in its own `info` map; an
unresolvable image is a real, empty response; no image specified at
all is a real `InvalidArgument` error — all checked over the real
wire protocol via the shared, generated `tonic` client
(`oci_cri_types::image_service_client::ImageServiceClient`), not just
in-process unit tests.

## Tests

Seven new unit tests directly in `crates/oci-store/src/resolve.rs`.
Two new in-process unit tests in `bin/ocicri/src/image_service.rs`
(`uid_and_username`'s own real edge cases; the no-image-specified
error, which needs no store access at all). Seven new real, socket-
connecting integration tests in `tests/tests/ocicri_image_service.rs`.
Every one of `ociman`'s own pre-existing image-resolution tests
(`rmi`/`inspect`/`tag`/`push`/`prune`) continues to pass completely
unmodified, confirming the shared-crate extraction changed no observed
behavior at all.

Full `cargo build --workspace --locked`/`cargo test --workspace
--locked` (2 clean runs, 93/93 result blocks — one new test binary,
`ocicri_image_service.rs`; `oci-store` now 29 tests up from 22,
`ocicri` now 4 up from 2)/`cargo fmt --all --check`/`cargo clippy
--workspace --all-targets --locked -- -D warnings`/`python3 ci/
guards.py`/`cargo deny check`/`bash ci/native-ci.sh` all clean. No
performance regression to any other binary (`ociman run --rm`, ~70ms,
within this project's own previously-observed noise band).

## What this doesn't do yet

`PullImage` (maps cleanly onto the already-shared `oci_registry::
pull_unconditionally`, matching CRI's own unconditional-pull
semantics — real cri-o has no separate pull-policy concept at the CRI
layer at all), `RemoveImage` (real CRI semantics: removing one tag
removes every tag/digest resolving to the same image, an idempotent
no-op if already gone — this project's own `oci_store::remove_image`
is pointer-only, matching that shape already, but the "remove every
sibling" loop itself isn't wired up yet), `ImageFsInfo` (no precedent
in `ociman` at all yet; would compose `store.blobs_dir()` +
`oci_store::dir_size`), and `StreamImages` (the streaming alternative
to `ListImages`, feature-gated in the real spec) are all real,
substantial, still-ahead future increments.
