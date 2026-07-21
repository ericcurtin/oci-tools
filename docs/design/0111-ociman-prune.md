# Design note 0111: `ociman prune` — reclaiming disk space for real

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Prune`, `cmd_prune`,
`PruneResult`); `crates/oci-store/src/rootfs_cache.rs`
(`prune`/`CachePruneReport`/`dir_size`); `crates/oci-store/src/lib.rs`
(re-exports); `bin/ociman/src/rootfs_setup.rs` (`cache_root` promoted
to a small shared helper).

## Why this, now

0110 wired a real overlay-based rootfs into `ociman run`, closing (and
reversing) the performance gap 0107 measured — but it also turned the
rootfs-cache 0109 built into something *actively* populated for the
first time: every distinct image manifest `ociman run` has ever used
now leaves a real, fully-extracted, uncompressed cache entry on disk,
forever, with nothing that ever cleans one up. Confirmed directly, not
assumed: pulling and running a real `docker.io/library/busybox:latest`
once left a ~4 MB cache entry behind after removing the image's own
tag with `ociman rmi` (which — correctly, matching real `docker rmi`/
`podman rmi`, see 0102 — only ever removes the tag *pointer*, not the
underlying blobs or this project's own newer rootfs cache). This
project's own repeated standard ("ensure we don't run out of disk
space") makes this a real, now-active risk worth closing directly,
not a hypothetical one.

Separately, `oci_store::Store::gc` (real, mark-and-sweep blob garbage
collection) has existed and been unit-tested since milestone 2 but was
never wired to any `ociman` command at all — the same gap this
increment closes for the rootfs cache turns out to already exist, one
level down, for blobs too.

## `ociman prune`: both reclamation passes, run only when asked

Matches real `docker system prune`/`podman system prune`'s own
convention exactly: reclaim disk space no longer needed by anything
currently tagged, but only when a user explicitly asks — never
implicitly folded into `rmi`/`rm`, which would tax *every* ordinary
removal with a full reachability scan for a benefit only worth paying
for occasionally. Two independent passes, reported separately (never
summed into one opaque total, since they reclaim two genuinely
different kinds of on-disk state for two different reasons):

* `store.gc()` — already-implemented, already-tested blob garbage
  collection, wired to a real command for the first time.
* `oci_store::prune` (new) — mark-and-sweep against a much smaller
  "reachable" set than blob GC needs: the rootfs cache is keyed
  directly by manifest digest, so "reachable" here is simply the
  digest half of every `Store::list_images()` record, no manifest/
  config/layer graph walk required the way blob reachability needs.
  Concurrency-aware the same way `Store::gc` already is: an in-
  progress `ensure_cached` build (a real `tempfile::tempdir_in` scratch
  directory, not yet renamed into place) is recognized by its own
  leading `.tmp` prefix and left alone rather than treated as an
  orphan.

## A real bug in the "how much did we reclaim" figure, caught by actually running it

Not assumed correct from the logic alone — checked directly, the same
standard this project always holds itself to: a first pass at
`dir_size` (summing every file's own `metadata().len()` recursively)
reported reclaiming **~490 MB** for a real busybox cache entry whose
own actual disk usage (`du`, before and after a real `ociman prune`)
was a few MB. The cause: real busybox images hardlink every applet to
one real binary (0106's own already-documented shape) — a naive walk
counts that one binary's own size again for *every* hardlinked name
pointing at it, not once. Fixed by tracking `(dev, ino)` pairs
(`std::os::unix::fs::MetadataExt`) and only counting a given real
inode's own size the first time it's seen; a new direct unit test
builds a real eleven-hardlinks-to-one-file layer and asserts the
reported size is exactly the one real file's own size, not eleven
times that — this specific regression can't silently reappear. This
only ever affected the *reported* figure, never the real correctness
of what got removed (`du` after a real prune already showed the right
answer even with the bug still in place) — but a project whose own
recurring standard is "real, checked, not assumed" numbers everywhere
else doesn't get to ship a wildly wrong one here either.

## Real, automated tests

`oci-store`'s own `rootfs_cache` module: 10 tests total (5 already
existing, 5 new) — an orphaned entry actually removed and reported;
a still-referenced entry kept untouched; an in-progress build's own
scratch directory surviving a concurrent prune; a missing cache root
handled as a real no-op, not an error; two images where only the
unreferenced one is removed; and the hardlink-dedup regression test
above. `tests/tests/ociman_prune.rs` (new, 4 tests): an empty store
reporting nothing to reclaim; a real orphaned blob actually removed
end-to-end through the real CLI (`ociman rmi` then `ociman prune`,
checked against the real on-disk store via `Store::has_blob`, not just
the CLI's own exit code); a still-tagged image's own blob surviving;
and — gracefully skipping itself if this specific host's own rootless-
overlay support (0108/0110) means `ociman run` never populated a cache
entry at all, rather than assuming it always will — a real orphaned
rootfs-cache entry actually removed the same end-to-end way. Manually
verified once more too, real `busybox`: `du` before prune (~6.0 MB) vs.
after (~28 KB), matching `ociman prune`'s own now-correct reported
`4170758` bytes almost exactly (the small remainder is the still-
referenced `blobs`/`images` pointer bookkeeping prune never touches).

## What this doesn't do yet

* No `--all`/selective pruning (real `docker system prune -a`'s own
  "also remove images with no container using them, not just
  fully-unreferenced blobs" mode) — this increment only ever removes
  what's *already* fully unreferenced (no tag resolves to it at all),
  matching real `docker system prune`'s own default (non-`-a`)
  behavior, not its more aggressive opt-in one.
* No automatic/scheduled pruning — matches the explicit "only when
  asked" design decision above, not a gap to close later.
