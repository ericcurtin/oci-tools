# Design note 0049: recording a committed layer into an image, and sharing RFC 3339 formatting (milestone 4)

Status: implemented (config/manifest recording only — no build
executor calls it yet)
Scope: `crates/oci-dockerfile/src/commit.rs` (new functions),
`crates/oci-spec-types/src/time.rs` (moved from `oci-runtime-core`).

0048's own "what's still not here" named this exact gap: *"nothing
updates an image config's own `rootfs.diff_ids`/`history` or a
manifest's own `layers` list with `commit_layer`'s own output —
trivial glue ... but no such caller exists yet."* This increment ships
that glue: `record_layer` and `record_empty_history`.

## Moving `format_rfc3339_utc` to `oci-spec-types`, not duplicating it

Both functions need a real RFC 3339 timestamp for `HistoryEntry::created`
(and `record_layer` for the config's own layer history). A hand-rolled,
dependency-free RFC 3339 formatter already existed —
`oci_runtime_core::time::format_rfc3339_utc`, written for
`PersistedState::created` (0004-era) — but it was crate-private to
`oci-runtime-core`, which `oci-dockerfile` has no reason to depend on
(no dependency cycle risk either way, but pulling in namespace/cgroup/
seccomp code transitively just to format a date would be exactly the
kind of unnecessary coupling this project's crate boundaries exist to
prevent).

Since `HistoryEntry`/`ImageConfig` (both needing `created` timestamps)
are defined in `oci-spec-types` — a crate `oci-runtime-core` *already*
depends on — moving the formatter there instead of duplicating it is
strictly better on every axis: no new dependency for `oci-dockerfile`
(already depends on `oci-spec-types`), no second copy of the same
hand-rolled civil-calendar math (`oci-runtime-core`'s own module doc
explicitly chose to hand-roll this specifically to avoid a `chrono`/
`time` dependency — duplicating that code would quietly reintroduce
the same "two implementations of one capability" problem `ci/
guards.py` polices for external crates, just not caught by tooling
since it'd be in-tree), and no crate boundary violation. `oci-runtime-
core::state`'s own call site now reads `oci_spec_types::
format_rfc3339_utc(...)` instead of `crate::time::...` — a pure,
mechanical move confirmed by running both crates' full test suites
unchanged (132 `oci-runtime-core` tests including the moved module's
own 4, all still passing) and a fresh `ocirun run` hyperfine
comparison (3.5ms mean, within this project's established ~2.6-3.1ms
noise band — this refactor never touches any hot path, `state.rs`
formats exactly one timestamp per container creation regardless of
which crate defines the function).

## `record_layer` / `record_empty_history`

```rust
pub fn record_layer(
    config: &mut ImageConfig,
    layers: &mut Vec<Descriptor>,
    committed: &CommittedLayer,
    created_by: impl Into<String>,
)

pub fn record_empty_history(config: &mut ImageConfig, created_by: impl Into<String>)
```

`record_layer` appends `committed`'s own `Descriptor` to `layers` and
its own `diff_id` plus a new non-empty `HistoryEntry` (timestamped
`SystemTime::now()`) to `config` — always all three together, so the
manifest's own layer list and the config's own `rootfs.diff_ids` list
can never drift out of the relative order the image-spec requires
between them (both bottom-layer-first).

`record_empty_history` is for instructions that change `config`'s own
runtime defaults without touching the rootfs at all (`ENV`/`LABEL`/
`CMD`/`WORKDIR`/`ARG`) — a history-only entry, `empty_layer: true`, no
`rootfs.diff_ids` entry — matching real `docker build`'s own `history`
shape exactly (`docker history` on any real image interleaves these
with real layer-producing entries).

`created_by` is deliberately just `impl Into<String>`, not a typed
"which instruction" enum: neither function has any idea yet what a
future build executor's own instruction-to-string rendering will look
like (real `docker build`'s own convention, shell-quoted `RUN /bin/sh
-c "..."`, is one reasonable future choice, not baked in here).

## Real, automated tests

2 new unit tests (plus the 3 from 0048, unaffected): `record_layer`
called twice keeps `layers` and `config.rootfs.diff_ids` in the same
relative append order, with real non-empty history entries carrying
the given `created_by` text and a real (loosely sanity-checked, not
pinned-to-one-instant) present-day timestamp; `record_empty_history`
touches only `config.history`, leaving `layers` and
`config.rootfs.diff_ids` untouched.

## Performance

`record_layer`/`record_empty_history` are not called from anywhere yet
(no build executor exists) — zero runtime impact by construction. The
`oci-runtime-core`/`oci-spec-types` timestamp refactor *does* touch
already-shipped, already-hot-path-adjacent code (`state.rs`), so it was
re-verified directly: a fresh `ocirun run` hyperfine run after this
change measured 3.5ms mean, within this project's established
~2.6-3.1ms noise band for the same benchmark, confirming no
regression.

## What's still not here

* Still no build executor — nothing decides *when* to diff a rootfs,
  drives a `RUN` step via `oci-runtime-core`, or calls `record_layer`/
  `record_empty_history` at all.
* Everything else 0039-0048 already listed as future work: `ONBUILD`/
  `HEALTHCHECK`, `--build-arg`, `COPY --from=<stage>` dependency
  resolution, the build cache, and `ociman build` itself.
