# Design note 0177: `ociman build --squash`

Status: implemented
Scope: `bin/ociman/src/build.rs` (`cmd_build`'s new `squash` parameter,
`build_stage`'s new `squash` parameter and its own post-instruction-
loop squash logic); `bin/ociman/src/main.rs` (`Command::Build`'s new
`squash` field); `tests/tests/ociman_build.rs`.

## Closing the gap 0174 explicitly named as the next increment

0174's own "what this doesn't do yet" section said, verbatim:
"`ociman build --squash` — a related but separate future increment...
left for later." This increment closes it.

## Real podman/buildah's own `build --squash` semantics — a genuinely
different shape from `commit --squash`, checked directly rather than
assumed identical

Before writing any code, a real `podman build --squash --format
docker` was run directly (busybox base, two separate `RUN`s plus a
`LABEL`) and inspected via `podman inspect .RootFS`/`podman history`.
This revealed a real, materially different behavior from 0174's
`commit --squash`:

* **Only the layers *this build itself adds* get folded into one** —
  the base image's own existing layers are left completely untouched
  and still individually present (`RootFS.Layers` had exactly 2
  entries: the base's own one layer, plus one new combined layer).
  `commit --squash` is different: it flattens the *entire* rootfs,
  including the base's own content, into one layer with no base
  layers referenced at all.
* **The full per-instruction history survives** — `podman history`
  still showed one entry per instruction (`RUN one`, `RUN two`,
  `LABEL`), each with its own original text, not collapsed to a single
  entry the way `commit --squash` collapses to exactly one. Only the
  *sizes* differ: every entry but the very last shows `size: 0`
  (`empty_layer` in this project's own terms); the last one alone
  carries the real, combined layer's own full weight.
* **Only the target stage is affected.** A multi-stage Containerfile
  where an earlier stage feeds the final one via `COPY --from=`
  showed the earlier stage building completely normally (its own real
  per-instruction layer), with only the *final* stage's own newly
  added layers folded together (`RootFS.Layers` still just 2 entries
  total: base + one combined layer for the target stage).
* **A stage with instructions that never touch the filesystem at all**
  (just `LABEL`) still gets one new, real (if empty) layer added —
  matching `commit_layer`'s own already-established "an empty diff
  still commits a real layer" convention.
* **A Containerfile with *no* instructions beyond `FROM` itself** is a
  true no-op: the built image's own ID comes back byte-identical to
  the base's (confirmed: same digest, same `RootFS.Layers`) — nothing
  to squash since this build added nothing at all.
* **The build cache is bypassed entirely.** Re-running an otherwise-
  identical `--squash` build a second time (no `--no-cache`) still
  re-executed every `RUN` (no `--> Using cache` in the build log) —
  real podman's own squashed-build layers are never stored as
  independently reusable layers to begin with.

## Design: a diff computed once per stage instead of once per
instruction, with no changes needed to any instruction's own execution
logic at all

Given the above, the implementation reuses the exact same primitives
`commit_layer`/`oci_layer::{Snapshot,changes}` already provide, just at
a different granularity — a real, checked-directly-safe design choice
over the alternative (suppressing each instruction's own individual
layer commit and only recording history): every `RUN`/`COPY`/`ADD`
instruction in `build_stage`'s own loop runs **completely unmodified**
— `apply_instruction`/`run_instruction`/`copy_instruction`/
`add_instruction` needed zero changes at all, keeping the blast radius
of this change confined to `build_stage`'s own post-loop code and
`cmd_build`'s own call site, not the much larger, more heavily tested
instruction-execution surface.

`build_stage` now:

1. Captures one extra [`oci_layer::Snapshot`] (`before_stage_snapshot`)
   right where every individual instruction's own "before" snapshot
   would otherwise start from — right after the base rootfs is
   populated and `/etc/hosts` is synthesized, right before the
   instruction loop begins — but only when `squash` is set.
2. Forces `needs_rootfs` to `true` whenever `squash` is set and the
   stage has *any* instructions at all (even metadata-only ones like
   `LABEL`), matching the "still adds one empty layer" behavior above;
   a stage with *zero* instructions never forces a rootfs at all,
   matching the observed true-no-op behavior for a bare `FROM`.
3. Runs every instruction exactly as before (each `RUN`/`COPY`/`ADD`
   still commits and records its own real, individual layer along the
   way, same as a non-squash build) — the per-instruction layers this
   produces are real, valid, already-stored blobs; they just never end
   up referenced by the final manifest once step 4 runs, and become
   ordinary orphaned blobs `ociman prune`'s own existing mark-and-sweep
   `Store::gc` already reclaims (the same "defer cleanup, never touch
   it eagerly" precedent every other build-scratch cleanup in this
   project already follows, e.g. `docs/design/0121`).
4. After the loop, if `squash` is set and this stage's own history
   actually grew (i.e. it had at least one instruction): diffs the
   *final* rootfs state against `before_stage_snapshot` once, commits
   that one combined diff as a single new layer via the exact same
   `commit_layer`, truncates `layers`/`config.rootfs.diff_ids` back
   down to the base's own count and appends just that one layer, and
   flips every history entry *this stage* added to `empty_layer: true`
   except the very last, which keeps `empty_layer: false` — matching
   real podman's own exact `podman history` display shape (checked
   directly, see above).

`cmd_build` only ever passes `squash: true` for the one stage that is
the actual build target (`squash && stage_index == target`); every
other stage in `build_order` always gets `squash: false`, matching the
"only the target stage is affected" behavior above. `--squash` also
forces `cache_candidates` to empty for the whole build (the same
effect `--no-cache` already has), matching the observed cache-bypass
behavior.

## Verified against real `podman build --squash`

This project's own implementation was run directly against the exact
same four Containerfile shapes real `podman build --squash` was
checked against during design (two-`RUN`s-plus-`LABEL`, metadata-only,
bare-`FROM`, and a `COPY --from=`-fed multi-stage build) and produced
matching layer counts in every case — including one striking
byte-for-byte match: the metadata-only stage's own empty squashed
layer digest (`sha256:5f70bf18...`) was identical to real podman's own
independently-produced digest for the exact same input, confirming
both implementations converge on the same real, gzip-compressed empty-
tar bytes. Every squashed image was also run directly to confirm real
content correctness (files from multiple folded `RUN`/`COPY` steps all
present and correct).

## Tests

`tests/tests/ociman_build.rs` gained 5 integration tests, each
verified against a real running container built by this project's own
`ociman build --squash`: folding two `RUN`s plus a `LABEL` into one
layer with the full history/`empty_layer` shape intact and both files'
content surviving; only the target stage of a `COPY --from=`-fed
multi-stage build being affected; a metadata-only stage still adding
one real empty layer; a bare-`FROM` Containerfile being a true no-op
(byte-identical manifest/history to the base); and the build cache
being genuinely bypassed (verified via a real, unique-per-execution
`/proc/sys/kernel/random/uuid` marker, not `date`'s own unsupported-by-
busybox `%N` nanosecond specifier, which was tried first and rejected
once checked directly against `busybox date --help`/a real invocation
showing it prints the literal string `%N` rather than expanding it).
Full `cargo build --workspace --locked`/`cargo test --workspace
--locked` (2 clean runs)/`cargo fmt --all --check`/`cargo clippy
--workspace --all-targets --locked -- -D warnings`/`python3
ci/guards.py`/`cargo deny check`/`bash ci/native-ci.sh` all clean.

## What this doesn't do yet

* Exactly matching real podman's own "FROM <base>" comment on the
  squashed layer's own history entry — a deliberate, documented
  omission (this project's own established "functional correctness
  over exact string content" convention, same as 0174's own identical
  choice for `commit --squash`'s history text): describing "which
  base" cleanly for both an external-image base and an in-memory
  earlier-stage base added real complexity for a purely cosmetic,
  informational touch.
* `--squash-all` (real buildah/podman's own separate multi-stage-wide
  flag, squashing every stage's own contribution, not just the
  target's) — a real, separate flag this project doesn't parse at all
  yet; out of scope for this increment.
