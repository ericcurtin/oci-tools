# Design note 0303: `ociman images --filter containers=true|false`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_images.rs`.

## Closing part of `0295`'s own "still ahead"

`0295` flagged `readonly=`/`intermediate=`/`containers=` as the
remaining `ociman images --filter` keys. Of the three, `containers=`
is the one clean fit for a single small turn:

- `readonly=` (checked directly, `~/git/container-libs/common/
  libimage/filters.go`'s own `IsReadOnly`) ties to real podman's
  "additional image stores" concept — a secondary, read-only store
  layered on top of the primary one. This project's `oci_store::Store`
  has no such concept at all (a single read-write store), so
  implementing this filter would mean inventing a whole new storage
  concept just to have something to check — left alone.
- `intermediate=` (real podman's `isIntermediate`: untagged AND has at
  least one real child image, `areParentAndChild` comparing
  `RootFS.DiffIDs` prefixes and `History` lengths) is real and
  buildable with data this project's `ImageConfig` already carries
  (`rootfs.diff_ids`/`history`), but needs a first-ever parent/child
  image relationship computation across every local image — a
  materially bigger, separately-scoped lift, left for its own future
  note.
- `containers=` (this note) needs no new concept and no new
  computation at all: this project already has the exact "does any
  container reference image X" primitive, used twice already
  (`rmi_one`'s own dependents check, `ociman prune`'s/`system df`'s
  own `in_use_digests` set).

## Sharing the existing primitive a third time

Found `ociman prune` and `ociman system df` each independently
computing the identical "every image digest any container currently
references" `HashSet`, byte-for-byte the same loop in both places.
Rather than writing a third copy for this filter, factored it out into
one shared `images_in_use_digests(store, containers)` function — the
same "share as much Rust code as possible" pattern this project
already follows elsewhere (`0295`'s glob-matcher move,
`resolve_by_reference_or_id`). `ociman prune`/`ociman system df` now
call this one function instead of each keeping their own copy; `0303`
is its third caller. (`cmd_system_df_verbose`'s own similar-looking
`containers_per_digest: HashMap<Digest, u64>` needs a real *count*,
not just membership, so it's left as its own, genuinely different
computation — not a fourth duplicate of this one.)

## A real, checked-directly stricter value rule

Read real podman's own `(*Runtime).containers`/`filterContainers`
directly (`~/git/container-libs/common/libimage/filters.go`) rather
than assuming symmetry with this project's own existing `dangling=`
parser: `containers=` accepts **only** the literal strings `"true"`/
`"false"` (no `"1"`/`"0"` shorthand, no case variants) or `"external"`
— a real, checked-directly *stricter*, different rule from
`dangling=`'s own (`strconv.ParseBool`-backed, accepting many more
forms). A dedicated `try_parse_containers_filter` matches this exact
narrower rule instead of reusing `try_parse_dangling_filter`.
`containers=external` (real podman's own "all associated containers
are external/non-managed ones" case) gets its own clear, honest error
naming this project's total lack of an external-container concept,
rather than silently accepting or misinterpreting it.

## Verified

Manual, end-to-end (real seeded busybox image): `ociman images
--filter containers=false` lists it before any container exists;
`ociman create` a container from it; `--filter containers=true` now
lists it, `--filter containers=false` no longer does. Cross-checked
directly against a real installed `podman 4.9.3` — identical observed
behavior for `containers=true`/`false`/an invalid value (`Error:
unsupported value "bogus" for containers filter`).

Integration (`tests/tests/ociman_images.rs`, 2 new tests): two
genuinely distinct images (different `Cmd`, so different manifest
digests — same "ensure real digest distinctness" technique `0295`'s
own reference-filter test already established) correctly split by
`containers=true`/`false` once one has a real container; an invalid
value and `containers=external` are both clear, distinct errors.

Regression: all 10 `ociman_images.rs` tests pass (8 pre-existing + 2
new); all 25 `ociman_prune.rs` and all 11 `ociman_system_df.rs` tests
still pass unmodified after the `images_in_use_digests` refactor; full
`cargo test --workspace --locked` (111 test result blocks, 0
failures).

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).

Performance: `ociman images` is not part of the hot-path benchmarks
tracked in `docs/benchmarks.md`; the shared `images_in_use_digests`
refactor is a pure extraction of already-existing code (same number of
operations, same call sites, just factored into one function) — no
behavior or performance change for `prune`/`system df`'s own existing
callers. No re-benchmark needed.

## Still ahead

`ociman images --filter intermediate=` (needs a first-ever parent/
child image relationship computation, real and buildable but a
separately-scoped, bigger lift) and `readonly=` (needs a whole new
secondary-store concept this project doesn't have — not a good fit as
currently scoped) remain the two real, open items from `0295`'s
original list.
