# Design note 0184: `ociman build --squash-all`

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Build`'s new `squash_all`
field); `bin/ociman/src/build.rs` (`cmd_build`'s mutual-exclusivity
check, `build_stage`'s new `squash_all` parameter and post-loop logic,
and a real bug fix in `clone_cache_tree`/`clone_cache_tree_inner`);
`crates/oci-dockerfile/src/commit.rs` (cosmetic only); `tests/tests/
ociman_build.rs`.

## Closing the gap 0177 explicitly named as its own follow-up

0177's own "what this doesn't do yet" section named `--squash-all` as
"a real, separate flag this project doesn't parse at all yet" — the
direct follow-up to `--squash` itself. This increment closes it.

## Real podman's own `--squash-all` semantics, checked directly —
genuinely different from `--squash`, not just an alias

Before writing any code, `podman build --squash-all` (the installed
4.9.3 supports it) was run directly against the same four Containerfile
shapes 0177 already checked for `--squash`:

* **Two `RUN`s**: `RootFS.Layers` has exactly **one** entry (not
  base-plus-one like `--squash`) — the base image's own layers are
  folded in too. History still shows one entry per instruction
  (`RUN one`/`RUN two`), but critically the base's own *inherited*
  history (`BusyBox 1.38.0 ...`) is **discarded entirely**, unlike
  `--squash`, which keeps it.
* **Multi-stage** (`COPY --from=<earlier-stage>`): only the *target*
  stage is affected, same as `--squash` — an earlier stage still
  builds completely normally.
* **Bare `FROM`, no other instructions**: unlike `--squash`'s own true
  no-op for this shape (byte-identical to the base), `--squash-all`
  *always* produces a real, freshly-recompressed single layer (a
  different digest than the base's own original layer) plus exactly
  one new, synthetic history entry — there is no cheap "nothing to do"
  shortcut once the base itself is being folded in too.
* `podman build --squash --squash-all` together is a clear, immediate
  error ("cannot specify --squash with --layers and --squash-all with
  --squash").

## Implementation: minimal new logic, reusing 0174's own primitive

`oci_dockerfile::squash_layer` (built for `ociman commit --squash`,
0174) already does exactly the "whole current tree, no base layers
referenced" operation `--squash-all` needs — reused directly in place
of `--squash`'s own diff-against-a-snapshot approach.

`build_stage`'s existing squash post-processing (0177) already tracked
`base_layer_count`/`base_history_count` to know which suffix of
`layers`/`config.history` a stage's own instructions had added.
`--squash-all` reuses that exact same bookkeeping, just discarding the
*prefix* (the base's own contribution) instead of keeping it: truncate
`layers`/`config.rootfs.diff_ids` to `0` instead of `base_layer_count`,
and `config.history.drain(..base_history_count)` before applying the
same "flip `empty_layer` on all but the last entry" logic 0177 already
established. A bare-`FROM` stage (zero new history entries even after
draining) gets one synthetic entry instead, matching the real,
checked-directly always-one-new-layer behavior above.

`needs_rootfs` is forced unconditionally for `--squash-all` (even a
bare `FROM`), unlike `--squash`'s own "only if this stage has at least
one instruction" shortcut — checked directly, since there's no
byte-identical-to-base case to skip.

## A real bug found and fixed along the way: `clone_cache_tree` never
preserved hardlinks

The very first working `--squash-all` build hung indefinitely instead
of completing. Bisected directly (temporary `eprintln!` instrumentation,
since this sandbox's own `ptrace_scope` blocks attaching a debugger):
`oci_layer::export_tree` itself completed in well under a second, but
produced a **490MB** tar for a Containerfile whose own real content is
under 2MB — the exact same order-of-magnitude symptom 0169's own
mount-boundary bug had, but a completely different real cause this
time. Direct inspection of the build's own scratch rootfs showed
`/bin` alone at 466MB — busybox's own ~380 applets, each written out
as a full, independent copy instead of the real hardlinks they should
be (all pointing at one real ~1.2MB binary).

Root cause, found by reading `clone_cache_tree` (the function that
clones the per-manifest-digest rootfs *cache* into a build's own fresh
scratch directory whenever the base is an external image, 0112)
directly: its own plain-file branch was a bare `std::fs::copy`, with
no hardlink-awareness at all — silently exploding every hardlinked
source file back apart into an independent copy on every single clone.
This directly contradicts the function's own doc comment ("the exact
same result `oci_layer::apply` would have produced" — which *does*
preserve hardlinks for a real tar `Link` entry, unchanged since 0169).

This was never a `--squash-all`-specific problem: it's a real, silent
disk-space cost every single build using a hardlink-heavy base image
was already paying, squash or not — `--squash-all` was simply the
first code path to ever need to walk/export the *entire* current tree
(rather than only ever diffing it), which is what finally made the
problem visible. Fixed by giving `clone_cache_tree` the exact same
`(dev, ino) -> already-cloned destination path` hardlink-tracking
`oci_layer::write_entry` already established (0169): a source file with
`nlink() > 1` that's already been seen gets a real `std::fs::hard_link`
to the earlier destination instead of a second independent copy.
Verified directly: an ordinary (non-squash) build's own scratch rootfs
for the same busybox base dropped from what would have been ~465MB to
a real ~4.2MB — the same real disk-space win 0169 already delivered
for `ociman export`, now also applying to every build's own scratch
directory.

## Tests

New unit test (`build.rs`): `clone_cache_tree` preserves a real
hardlink (same inode on both sides after cloning), not an independent
copy. New integration tests (`ociman_build.rs`): `--squash-all` folds
the base in too, leaving exactly one layer and discarding the base's
own inherited history entirely (only this build's own two `RUN`
instructions survive); a bare-`FROM` `--squash-all` still produces one
real, freshly-recompressed layer and one synthetic history entry
(never the base's own original digest, unlike `--squash`'s own true
no-op there); only the target stage of a multi-stage build is
affected; and `--squash`/`--squash-all` together is a clear error. All
verified against a real running container built by this project's own
`ociman build --squash-all`, and manually cross-checked against the
same shapes run through a real `podman build --squash-all` during
design. Full `cargo build --workspace --locked`/`cargo test --workspace
--locked` (2 clean runs)/`cargo fmt --all --check`/`cargo clippy
--workspace --all-targets --locked -- -D warnings`/`python3
ci/guards.py`/`cargo deny check`/`bash ci/native-ci.sh` all clean.

## What this doesn't do yet

Nothing new — this closes 0177's own last-named follow-up completely;
0177's other original "what this doesn't do yet" items (exactly
matching real podman's own history-entry comment text, `ADD --exclude`
unrelated to squash at all) still apply exactly as before.
