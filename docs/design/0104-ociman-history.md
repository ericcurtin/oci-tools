# Design note 0104: `ociman history` (milestone 2/3/4)

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::History`, `cmd_history`,
`HistoryEntryView`, `history_layer_sizes`), `tests/tests/ociman_history.rs`.

Following 0102 (`rmi`) and 0103 (`tag`), `history` rounds out this
project's basic image-inspection surface: matching real `docker
history`/`podman history`, showing an image's own recorded
`ImageConfig.history` (already populated by every `ociman build`
instruction since `commit.rs`'s own `record_layer`/`record_empty_
history`, milestone 4) newest-layer-first, with each row's own real
stored (compressed) size.

## The one real design decision, and the bug it caught before shipping

`ImageConfig.history` has everything a row needs except a byte size,
which lives on the *manifest*'s own `layers` list instead — one entry
per **non**-empty-layer history entry, in the same bottom-layer-first
relative order (the same correspondence `ociman build`'s own local
build cache, `build_cache.rs`'s `find_cached_layer`, already relies on
for cache-hit lookups, 0101).

The subtlety: `history` is not guaranteed to describe *every* real
layer. A base image pulled from a real registry (or, in this project's
own test suite, `seed_image`'s deliberately bare fixture) commonly has
one or more real layers with *no* history entry at all. Since
`record_layer` only ever *appends* to `history` and `rootfs.diff_ids`/
`layers` together, any undescribed layer can only ever be one of the
*earliest* (bottommost) ones — never interspersed with described ones
later in the list. So the correct starting index for walking
`manifest.layers` in lockstep with `history`'s own non-empty entries is
`layers.len() - non_empty_count`, not `0`.

This was not a hypothetical worth a comment and nothing else: a first
pass at `cmd_history` started the walk at index `0` (as if every layer
always had a description), and the very first real integration test
(`RUN` then `ENV` on top of a bare `seed_image` base — exactly the
"one undescribed base layer, then two ociman-build-native entries"
shape) caught it immediately: the `RUN` layer's own reported size came
back as the *base* layer's own (much larger) size instead of its real
one. Fixed by computing the offset from the *end* rather than
defaulting to the start, then factored the whole computation out into
a small, pure `history_layer_sizes` function specifically so this
alignment logic has its own direct, real-store-independent unit tests
(`tests::history_layer_sizes_*` in `main.rs`) in addition to the
integration test that first found it.

## Real, automated tests

`tests/tests/ociman_history.rs`: a real `ociman build` (`RUN` then
`ENV` over a bare seeded base) confirms newest-first ordering, correct
per-row sizes (the metadata-only `ENV` entry reports `0`, the `RUN`
entry reports its own real compressed layer size, not the base
layer's), and that the human-readable table and `--json` output agree;
an unknown-reference error; and a bare `seed_image`-only image (no
history at all) reporting `"no history"`/an empty JSON array rather
than erroring. Four focused unit tests on `history_layer_sizes` itself
cover the full lockstep case, the undescribed-base-layer case (the
exact regression above), an image with no history at all, and an
image whose history is entirely metadata-only entries.

## Performance

No hot path touched — image history is an infrequent, offline
inspection operation (reading two already-local JSON blobs), not part
of any startup/destroy-time benchmark this project's own README goal
cares about.
