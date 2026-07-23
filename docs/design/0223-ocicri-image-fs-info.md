# Design note 0223: `ocicri` `ImageService.ImageFsInfo`

Status: implemented
Scope: `crates/oci-store/src/rootfs_cache.rs` (`dir_stats`, new);
`bin/ocicri/src/image_service.rs` (`filesystem_usage`, `image_fs_info`).

## The last real, small `ImageService` gap

`ListImages`/`ImageStatus`/`PullImage`/`RemoveImage` (0213-0215) left
`ImageService` with two RPCs remaining: `ImageFsInfo` and
`StreamImages`. `StreamImages` is a feature-gated (`CRIListStreaming`,
checked directly in the proto's own doc comment), opt-in streaming
alternative to `ListImages` that real `cri-o` itself may not even
implement — real streaming-RPC plumbing for a rarely-used, gated
feature, left for its own separate increment. `ImageFsInfo` is the
opposite: small, genuinely useful (a real kubelet calls this
routinely for node-level disk-pressure eviction decisions), and,
checked directly against real `cri-o`'s own implementation
(`server/image_fs_info.go`, `utils.GetDiskUsageStats`), almost entirely
bookkeeping — a directory-tree walk summing byte sizes, never
`statfs(2)`, no real namespace/mount work at all. A safe, narrow next
slice.

## What real `cri-o` actually computes (checked directly, not guessed)

- `used_bytes`/`inodes_used`: a real `filepath.Walk`, summing each
  entry's own `info.Size()` and counting every visited node once
  (including directories) — never `statfs(2)`, never a `du`-style
  block count, and (a real, cruder-than-this-project's-own gap) no
  hardlink deduplication at all.
- `timestamp`: literally `time.Now().UnixNano()` at response-build
  time, never derived from any file's own mtime.
- Exactly one `FilesystemUsage` entry in each of `image_filesystems`
  and `container_filesystems`, always — no per-storage-graph-driver
  fan-out (only one is ever active).
- `container_filesystems` is always populated too in the common case,
  not left empty — cri-o reuses the identical walk against a second,
  separate graph-driver directory.

## `oci_store::dir_stats`: the same real walk `dir_size` already does, plus a count

`oci_store::rootfs_cache::dir_size` (0111/0121) already computes a
real, hardlink-deduplicated byte total. `ImageFsInfo` needs a count
too (`inodes_used`) — rather than write a second, independent walk,
`dir_size`'s own internal helper now returns `(bytes, files)` and
`dir_size` itself is a one-line wrapper discarding the count, so every
existing caller (`ociman prune`'s own reclaimed-bytes reporting, this
crate's own tests) is completely unaffected. Deliberately *more*
correct than real cri-o's own cruder walk here — this project's own
already-established, previously-bug-fixed hardlink-dedup (0106/0111)
applies to both `image_filesystems`/`container_filesystems` alike,
not re-introduced or worked around.

## The real "which two directories" mapping

- `image_filesystems`: `Store::blobs_dir()` — this project's own real,
  content-addressed blob store, every image's actual on-disk bytes.
- `container_filesystems`: `oci_store::cache_root` — this project's
  own real, extracted rootfs cache, the same directory `ociman run`/
  `ociboot build-image` themselves extract into and already share
  (0110/0200). The closest real analogue to cri-o's own separate
  "container filesystem" figure this project actually has: running
  containers are backed by that cache, not the blob store directly.

## One deliberate divergence from real cri-o's own error behavior

Real cri-o's own test suite expects a hard error when the target
directory doesn't exist at all. `oci_store::cache_root` legitimately
doesn't exist yet on a store nothing has ever `run`/`build-image`d
anything on (unlike `blobs_dir`, which `Store::open` always creates
eagerly) — a real, ordinary, expected state, not misconfiguration.
Treating that the same way cri-o treats a genuine, unexpected error
would make a perfectly healthy, freshly-initialized node's own
`ImageFsInfo` call fail for no real reason. `filesystem_usage` reports
a real, honest all-zero `FilesystemUsage` for exactly `ErrorKind::
NotFound` and nothing else — any other I/O error (permission denied,
...) still propagates as a real, hard failure, matching cri-o's own
general "can genuinely fail" contract.

## Verified

- Two new unit tests in `oci-store` (`dir_stats` matches `dir_size`'s
  own byte total while also reporting a correct, hardlink-deduplicated
  file count; an empty directory is a real zero, not an error).
- Two new integration tests in `tests/tests/ocicri_image_service.rs`:
  a real seeded image reports genuine, nonzero `used_bytes`/
  `inodes_used` for `image_filesystems` (an honest lower-bound check,
  not a pinned exact byte count — this project's own more-precise
  hardlink-dedup shouldn't be re-derived under test, just confirmed
  positive) while `container_filesystems` is a real, legitimate zero
  (nothing has extracted a rootfs yet); a completely empty store
  reports real zeros across the board for both fields, not an error —
  directly exercising the "`cache_root` doesn't exist yet" path.
- Full workspace: `cargo build`, `cargo test --workspace` (95/95
  result blocks — `oci-store`'s own block grew 29→31,
  `ocicri_image_service`'s own grew 11→13, everything else unchanged —
  0 failures), `cargo fmt --check`, `cargo clippy --all-targets -- -D
  warnings`, `python3 ci/guards.py` (18 capability groups, unaffected),
  `cargo deny check`, `bash ci/native-ci.sh`, hyperfine perf sanity on
  `ociman run --rm` (no regression — this change never touches
  `ociman`/`ocirun`'s own hot path at all, only `oci-store`'s already-
  shared `rootfs_cache` module and `ocicri` itself).

## What's still not here

`StreamImages` (still a real, honest `Status::unimplemented`, see
above for why). Every `RuntimeService` pod-sandbox/container-lifecycle
RPC remains unimplemented — `ImageService` is now feature-complete
apart from that one gated, streaming alternative.
