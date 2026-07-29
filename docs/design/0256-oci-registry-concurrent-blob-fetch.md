# Design note 0256: bounded-concurrency layer fetch in `oci-registry::pull`

Status: implemented
Scope: `crates/oci-registry/src/pull.rs`, `crates/oci-registry/src/client.rs`.

## Finishing already-planned scope

`crates/oci-registry/src/lib.rs`'s own module doc comment has listed
"bounded-concurrency layer fetch" under "Planned (later milestones)"
since early in the project — `pull()`'s own loop fetched the config
blob and every layer one at a time, over the single `Client` the
caller supplied. This is the one place in the codebase where real,
currently-serialized *network* I/O sits on `ociman pull`'s hot path:
`oci-layer`'s tar extraction and `oci-store`'s blob I/O are both
already streaming and already tuned (`docs/design/0106`/`0111`), and
`oci_store::rootfs_cache::ensure_cached`'s own per-layer *apply* loop
must stay sequential (whiteout semantics need strict stack order) —
but nothing about *fetching* independent, content-addressed blobs from
a registry has any such ordering dependency.

## What changed

`fetch_blobs_concurrently` (`pull.rs`) replaces the flat `for layer in
&manifest.layers` loop: the config digest plus every layer digest are
combined into one list and distributed across up to
`MAX_CONCURRENT_BLOB_FETCHES` (3, matching real Docker's own
`max-concurrent-downloads` default exactly — the one real, checked
number to pick rather than an arbitrary one) simultaneous connections.
The caller's own already-authenticated `Client` handles the first
digest inline (no thread at all); every other worker gets its own
independent `Client` via a new `Client::duplicate_for_worker()` (same
credentials, same insecure-host set, a fresh connection pool and empty
token cache — deliberately not `Clone`, since that name would wrongly
imply the pool/cache come along too). A single-blob image (the
overwhelmingly common case for a tiny test/base image) takes the exact
same one-fetch, zero-extra-thread path as before; the bound only
matters once there's more than one blob to fetch at all.

`oci_store::Store` needed no changes at all: every method takes
`&self`, and `ingest_verified` already writes to a uniquely-named temp
file and atomically renames into place, so concurrent ingestion (even
of the *same* digest, shared between two images) was already
race-free by construction.

The first real error stops every worker from starting a *new* fetch (a
shared `AtomicBool`); an already-in-flight fetch runs to completion
rather than being force-cancelled (this crate has no I/O-level
interrupt mechanism) — a small, harmless wasted transfer, not a
correctness issue. The first error is what `pull()` ultimately reports,
matching the original sequential loop's own fail-on-first-error
behavior exactly.

## Verified

- **Deterministic, non-flaky concurrency proof** (`pull.rs`,
  `pull_fetches_multiple_layers_concurrently_not_sequentially`): a real
  mock registry (now handling each connection on its own thread,
  needed so it can actually be hit concurrently rather than
  accidentally serializing callers itself) with four layers, each
  artificially delayed 200ms. Sequential would take ≥800ms;
  bounded-concurrency-over-3-workers finishes in two real rounds
  (~400ms) — asserted under a generous 600ms bound, comfortably below
  the sequential floor and well above the concurrent ideal, so this
  passes on real overlap, never on timing luck.
- **Real, measured, end-to-end speedup**: a real local `registry:2`
  instance seeded with a genuine 4-layer `python:3.12-slim` image,
  `hyperfine`-timed `ociman pull` (cold store every sample) before vs.
  after this change (git-stash A/B, this project's own established
  methodology): **1.48× faster** (54.6ms → 36.8ms mean, 8 runs each).
- Every existing `oci-registry` test (31 total) still passes unchanged,
  including the single-layer `pull_stores_manifest_config_and_layers`
  (which exercises the unchanged, zero-extra-thread fast path).
- Full workspace: `cargo build`/`test --workspace` (107 test
  binaries), `cargo fmt --all --check`, `cargo clippy --workspace
  --all-targets -- -D warnings`, `python3 ci/guards.py`, `cargo deny
  check`, `ci/native-ci.sh`, `ci/build-deb.sh`.

## Still ahead

`push()`'s own mirror-image blob-upload loop stays sequential for now
— parallelizing it needs `push()`'s own signature to grow enough
information to build additional `Client`s the way `pull()`'s internal
`client_for` already could (or a small, separate refactor to accept
one already built and duplicate it, as this slice's own
`duplicate_for_worker` does), deliberately left for its own follow-up
rather than growing this change's scope further. Registry mirrors and
fallback, retry with backoff, and resumable blob downloads/uploads
remain the crate's own other still-planned items.
