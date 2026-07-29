# Design note 0257: bounded-concurrency layer push in `oci-registry::push`

Status: implemented
Scope: `crates/oci-registry/src/push.rs`.

## `pull`'s own documented follow-up, now done

`docs/design/0256` parallelized `pull()`'s blob-*fetch* loop and
explicitly left `push()`'s mirror-image blob-*upload* loop for a
follow-up, since `push()`'s own signature (a caller-supplied `&mut
Client`, not one `pull()` builds internally via `client_for`) needed
`Client::duplicate_for_worker` (already added in 0256) before
additional worker clients could be constructed at all. That
prerequisite already existed, so this slice is almost pure repetition
of 0256's own pattern, not new design.

## What changed

`push_blobs_concurrently` (`push.rs`) replaces the flat `for layer in
&manifest.layers` loop the exact same way `pull`'s own
`fetch_blobs_concurrently` does: the config digest plus every layer
digest combined into one list, distributed across up to
`MAX_CONCURRENT_BLOB_UPLOADS` (5, matching real Docker's own
`max-concurrent-uploads` default — genuinely different from `pull`'s
own `MAX_CONCURRENT_BLOB_FETCHES` of 3, because real Docker itself
uses a different default for each direction, not a copy-paste
oversight). The caller's own client handles the first digest inline;
every other worker gets its own `Client::duplicate_for_worker()`. The
manifest itself is still pushed last, after every blob it references —
unchanged, and the one real ordering dependency this whole change
respects (a registry could otherwise serve a manifest naming a blob
that isn't there yet).

`oci_store::Store` needed no changes here either: `open_blob` opens a
fresh, independent `File` handle per call with no shared cursor or
mutable state.

## Verified

- **Deterministic, non-flaky concurrency proof**
  (`push_uploads_multiple_blobs_concurrently_not_sequentially`): the
  exact mirror of `pull`'s own equivalent test, including making
  `push.rs`'s own `MockRegistry` handle each connection on its own
  thread (needed for the same reason `pull.rs`'s mock needed it) and a
  new `start_with_upload_delay` — five blobs (config + four layers),
  each upload artificially delayed 200ms. Sequential would take
  ≥1000ms; bounded concurrency over 5 workers finishes in essentially
  one real round (~400ms measured, including the non-delayed
  HEAD/POST round trips each blob still makes) — asserted under a
  generous 700ms bound.
- **Real, measured, end-to-end speedup**: the same real local
  `registry:2` methodology as 0256, pushing a genuine 4-layer
  `python:3.12-slim` image to 10 distinct, never-before-uploaded
  repository tags per binary (git-stash A/B, avoiding registry-side
  dedup from masking a second push of the same tag): **1.81× faster**
  (206ms → 114ms mean, 10 runs each).
- Every existing `oci-registry` test (32 total) still passes
  unchanged, including the single-layer push tests exercising the
  unchanged fast path.
- Full workspace: `cargo build`/`test --workspace` (107 test
  binaries), `cargo fmt --all --check`, `cargo clippy --workspace
  --all-targets -- -D warnings`, `python3 ci/guards.py`, `cargo deny
  check`, `ci/native-ci.sh`, `ci/build-deb.sh`.

## Still ahead

Registry mirrors and fallback, retry with backoff, and resumable blob
downloads/uploads remain `oci-registry`'s own other still-planned
items (`crates/oci-registry/src/lib.rs`'s own module doc comment).
