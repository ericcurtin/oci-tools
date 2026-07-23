# Design note 0215: `ocicri` `ImageService.RemoveImage`

Status: implemented (first real slice — the real container-in-use
check real `cri-o` also has isn't ported, see below)
Scope: `bin/ocicri/src/image_service.rs` (`remove_image`);
`tests/tests/ocicri_image_service.rs`.

## Completing `ImageService`'s own basic CRUD

`ListImages`/`ImageStatus`/`PullImage` (0213-0214) plus this
increment's own `RemoveImage` round out `ImageService`'s own basic
read/pull/remove set — only `ImageFsInfo`/`StreamImages` remain
`Status::unimplemented` now.

## A real, checked-directly rule genuinely different from `ociman rmi`'s own

`ociman rmi <tag>` (an exact tag match) only ever removes that one
tag, leaving any sibling tag pointing at the same image alone —
removing *by ID* when more than one tag shares that digest needs
`--force` first (0122, a deliberate, checked-directly-against-real-
podman design choice). Real CRI's own `RemoveImage` is a genuinely
different, stricter rule, confirmed two ways: the proto's own comment
("removing the image by a single tag will remove all of its tags,
even across different repositories") and real `cri-o`'s own source
(`~/git/cri-o/server/image_remove.go`) — no `--force`-style ambiguity
gate at all (this RPC has no interactive confirmation to skip in the
first place, matching this project's own established `ocibox rm --all`
/`ephemeral` reasoning for the identical "nothing to skip" case).
`ocicri`'s own `remove_image` therefore does *not* call into anything
`ociman rmi` already has — it's a small, direct, from-scratch
implementation matching CRI's own real contract: resolve `spec` (tag
or a real/short ID, via the already-shared `oci_store::
resolve_by_reference_or_id`) to a manifest digest, then remove *every*
stored reference sharing that digest, unconditionally.

## Idempotent by design

Real CRI's own documented contract: `RemoveImage` "must not return an
error if the image has already been removed." Nothing resolving to a
real image at all is a real, silent success — not treated as an error
condition to special-case around, just the natural fall-through of "no
digest to remove anything under." An ambiguous *ID* prefix (matching
more than one genuinely *different* image, `oci_store::
StoreError::AmbiguousId`) is still a real error, correctly distinct
from "nothing resolved" — that's a real client-input problem (which
image did the caller actually mean?), not "already gone."

## What real `cri-o` also has but isn't ported here

Real `cri-o`'s own `RemoveImage` refuses to remove an image any
container still references (`volumeInUse`). Not implemented: this
project's own `ocicri` cannot create any container via CRI at all yet
— every `RuntimeService` pod-sandbox/container-lifecycle RPC is still
a real, honest `Status::unimplemented` — so there is currently no
possible "in use by a real CRI container" case to even check against.
A real, honest gap to close once container creation itself exists,
not a shortcut around something that matters yet.

## Verified by hand

A real seeded image removed by its own exact tag disappears from
`ListImages` afterward. A second, real sibling tag pointing at the
identical manifest digest (constructed the same way `ociman tag`
itself would) is removed too when only the *first* tag was named,
proving the "removes every sibling" rule genuinely differs from
`ociman rmi`'s own. Removing an image that was never there at all is a
real, silent success, never an error. No image specified is a real
`InvalidArgument` error.

## Tests

Four new real, socket-connecting integration tests in `tests/tests/
ocicri_image_service.rs` (exact-tag removal; sibling-tag removal;
idempotent no-op removal; missing-argument error). One new in-process
unit test (the argument-validation case, which needs no store access
at all).

Full `cargo build --workspace --locked`/`cargo test --workspace
--locked` (2 clean runs, 94/94 result blocks — no new test binaries,
just more tests in two already-existing ones: `ocicri_image_service.rs`
now 11 up from 7, `ocicri`'s own unit tests now 6 up from 5)/`cargo fmt
--all --check`/`cargo clippy --workspace --all-targets --locked -- -D
warnings`/`python3 ci/guards.py`/`cargo deny check`/`bash
ci/native-ci.sh` all clean. No performance regression to any other
binary (`ociman run --rm`, ~66ms, within this project's own previously
-observed noise band).

## What this doesn't do yet

`ImageFsInfo`, `StreamImages`, inline `PullImage` `AuthConfig`
credentials, and the real container-in-use check `RemoveImage` would
need once container creation exists are all real, still-ahead future
increments.
