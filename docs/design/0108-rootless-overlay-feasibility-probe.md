# Design note 0108: a real, tested rootless-overlay feasibility probe

Status: implemented (groundwork only — see "What this doesn't do yet")
Scope: `crates/oci-runtime-core/src/overlay.rs` (new module,
`rootless_overlay_supported`), `crates/oci-runtime-core/src/lib.rs`
(module registration). No existing behavior changes.

## Why this, now

0107's own "what this doesn't fix, and why not attempted here" section
named the real, measured gap (real `podman run` now 1.71× faster than
`ociman run` for a real multi-thousand-file image, because `ociman`'s
own no-overlay design still fully extracts every layer's own files
from scratch every run) as this project's own next real priority, and
explicitly deferred the fix rather than rushing it into the same
session as an unrelated compatibility bug.

This session picked that up — but rather than jumping straight to
wiring an overlay-based rootfs into `ociman run`'s own live container
path (a genuinely large, correctness-sensitive change touching mount
lifecycle, cache invalidation, and interaction with `--read-only`, all
at once), it starts with the one piece that can be built, tested, and
landed **completely safely, with zero risk to any existing container**:
a real, live probe for whether the current environment can actually do
what the eventual fix needs at all.

## What was verified, and how

A real, manual, unprivileged test on this session's own dev host
(kernel 6.17), matching exactly the namespace shape every rootless
container this project already creates:

```sh
unshare --user --map-root-user --mount bash -c '
  mount -t overlay overlay -o lowerdir=...,upperdir=...,workdir=... merged
  echo modified > merged/file.txt
'
```

Succeeded outright — the write landed in `upperdir`, `lowerdir`'s own
content stayed untouched, exactly the copy-on-write semantics a real
overlay-based rootfs would rely on. This is the same finding real
`podman`'s own `overlay2` graph driver already depends on (0107's own
documented explanation for *why* it beats `ociman` at scale) — proof
this project's own already-established rootless namespace model
(`unshare(CLONE_NEWUSER|CLONE_NEWNS)` + self `uid_map`/`gid_map`, see
`crates/oci-runtime-core/src/namespaces.rs`) can support the same
approach, with no *new* privilege model needed at all.

`rootless_overlay_supported(scratch_dir)` turns that manual shell test
into a real, reusable, safe Rust primitive: fork (so `unshare(NEWUSER)`
never corrupts the calling process's own namespaces — one-way
otherwise), unshare a fresh user+mount namespace in the child, map the
caller to root inside it, then attempt the real overlay mount — all of
it reusing already-existing, already-tested primitives
(`namespaces::unshare`/`write_id_mappings`, `oci_mount::
parse_mount_options`/`mount`, `process::fork_and_wait`) rather than any
new syscall-wrapping code.

## Two real bugs the first implementation attempt had, both caught by actually running it

Not assumed correct from reading the man pages — checked directly,
exactly this project's own established verification standard:

1. **`geteuid`/`getegid` were read *after* `unshare(CLONE_NEWUSER)`,
   not before.** After unsharing, the process's own view of its uid/gid
   is the fresh namespace's unmapped "overflow" id (`65534`), not the
   real caller's own id `write_id_mappings` needs to map *to* — so the
   very first attempt failed with a genuine `EPERM` writing
   `/proc/self/uid_map`, immediately, the first time this was actually
   run (not a hypothetical). Fixed by reading both ids before unsharing.
2. **A real overlay mount's own kernel-internal bookkeeping locks its
   `workdir/work` subdirectory to mode `0000`.** The probe's own
   cleanup (`remove_dir_all` over the scratch directories) silently
   failed to remove it — caught directly (`ls`/`find` both refusing
   `work/work` with a real permission error, even under the same uid
   that owns it) after the first successful mount, not assumed. Fixed
   by a `reset_permissions_for_removal` pass (`chmod 0700`,
   deepest-first, tolerant of errors) before the real removal —
   otherwise every use of this probe would leak an un-removable
   directory, exactly the kind of slow disk-space accumulation this
   project's own "ensure we don't run out of disk space" standard
   exists to catch.

Both were caught by writing a tiny, throwaway scratch binary (not part
of the committed test suite — see the next section for why an
automated `#[test]` can't exercise the real path directly) that called
the function directly and printed/inspected the real result, run
several times in a row to confirm the fix was solid, not a one-off.

## Why no automated test exercises the real (happy-path) probe

`unshare(2)` with `CLONE_NEWUSER` fails with `EINVAL` when the calling
*process* has more than one thread — `cargo test`'s own harness runs
every test on its own spawned thread, so calling into a function that
does this from a plain `#[test]` is unsound the same way
`crates/oci-runtime-core/src/namespaces.rs`'s own "Why no automated
syscall test" section already documents (a lock held by a *different*
thread of the parent at the moment of `fork()` could stay locked
forever in the forked child). `crates/oci-runtime-core/src/launch.rs`
— which does the identical fork-then-`unshare(NEWUSER)` dance for
every real container this project creates — has **zero** direct
`#[test]`s of its own for exactly this reason, relying entirely on
`tests/tests/ocirun_run.rs` spawning the real, compiled binary (a
genuinely fresh, single-threaded process from `main()`) instead. This
module follows the identical, already-established precedent: only the
one path safely testable without forking at all (a `scratch_dir` whose
subdirectories can't even be created, failing before the fork ever
happens) has a direct unit test; the real happy path is manually
verified (documented above) and deferred to a real end-to-end
integration test once a future increment actually wires this into a
CLI command.

## What this doesn't do yet

* **Nothing calls this function from any real container-creation path
  yet.** `ociman run`'s own rootfs setup is completely unchanged; this
  is intentionally pure groundwork, matching this project's own
  long-established "primitive lands as its own increment before a
  later one wires it in" pattern (e.g. 0039-0049's parser/diff/export/
  commit primitives, wired into `ociman build` for the first time only
  at 0050).
* No CLI surface exposes this probe's own result yet (deliberately —
  see the design note's own history: an earlier draft considered
  bundling it into `ocirun features`'s existing JSON output, but that
  would have made the *existing*, already-safe
  `features_serializes_with_the_real_spec_field_names` unit test
  hazardous the same way, since it calls `features()` directly from a
  plain `#[test]`; a real CLI integration needs its own dedicated,
  subprocess-spawning test, deferred to whichever future increment
  actually adds it).
* The actual fix (an overlay-based `ociman run`/`ociman build` rootfs,
  keyed by a per-image-manifest-digest "golden" cache as the
  `lowerdir`, a fresh per-container `upperdir`/`workdir`) is still not
  implemented. The clean integration shape this session's own research
  found: express it as one ordinary entry in the bundle's own
  `config.json` `mounts` list (`destination: "/"`, `type: "overlay"`,
  options carrying `lowerdir=`/`upperdir=`/`workdir=`) — `oci_runtime_
  core`'s own existing, already-generic mount-application code
  (`rootfs::plan_rootfs_setup`/`launch::execute_rootfs_action`)
  already handles an arbitrary `spec.mounts` entry with no runtime-core
  changes needed at all; only `ociman`'s own bundle synthesis
  (`cmd_run`) would need to build/reuse the golden cache and add that
  one mount entry instead of its own current per-layer `oci_layer::
  apply` loop. Left for a dedicated future increment given the real
  correctness surface a first cut needs to get right (concurrent cache
  population by two containers of the same image, graceful fallback
  to the existing extraction path in an environment this probe reports
  as unsupported, interaction with `--read-only`) — not something to
  rush into the same session as this groundwork.
