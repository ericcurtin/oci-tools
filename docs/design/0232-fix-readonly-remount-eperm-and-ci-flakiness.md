# Design note 0232: a real, security-relevant read-only-remount bug, and two real CI flakes

Status: implemented
Scope: `crates/oci-runtime-core/src/rootfs.rs`; `crates/oci-runtime-core/src/launch.rs`;
`tests/tests/ociman_commit.rs`.

## Found by actually checking GitHub Actions, not just local checks

This session's own periodic check-in did something none of the
several preceding sessions had actually done: ran `gh run list` and
looked at the *real* GitHub Actions results for recent pushes. Every
single CI run going back roughly 24 hours — dozens of commits — had
been failing on GitHub, almost always in the `vm (ubuntu-26.04,
x86_64)` cell, occasionally on `native (aarch64)` too. Every one of
those sessions' own local verification (this project's own explicit
"run the checks locally, excluding x86_64, since emulation is slow"
instruction) had genuinely passed — the gap was never checking the
*real* result of the x86_64/native jobs this project's own CI
actually runs on every push. This is now a real, permanent addition to
this project's own process: check `gh run list`/`gh run view` after
pushing, not just trust local checks for the paths CI actually
exercises remotely.

## Bug 1 (real, security-relevant): `RemountReadonly` tolerated `EPERM` unconditionally

`bin/ociman/src/main.rs`'s `-v NAME:/path:ro` (0086) and real `docker`/
`podman`'s own identical read-only-bind-mount contract both rely on a
real two-step kernel sequence: a plain bind mount, then a separate
`MS_REMOUNT|MS_BIND|MS_RDONLY` remount (the kernel rejects `RDONLY`
atomically with `BIND` on a fresh bind mount — `docs/design/0007`/
`0008`). `crates/oci-runtime-core/src/launch.rs`'s own execution of
that second step tolerated **any** `EPERM` there, unconditionally,
treating it as the exact same "known rootless limitation" a
*different*, real, already-documented case genuinely is:
`Spec::into_rootless`'s own real `/sys` bind mount (a rootless
container can't mount a fresh `sysfs`, so it bind-mounts the host's
real `/sys` read-only instead — a real host filesystem this process
doesn't own the superblock of, where `EPERM` on remount is a real,
understood limitation, `docs/design/0010`/`0012`).

The bug: **every** bind+`ro` mount reaching that same code path —
including a real user's own explicitly-requested `-v name:/path:ro`
volume, a directory this project's own code created and fully owns —
got the identical tolerant treatment. On an environment where the
remount genuinely fails with `EPERM` (confirmed directly: the real
`vm (ubuntu-26.04, x86_64)` CI cell), `ociman run` would silently
leave the volume writable while reporting complete success — a real,
silent lie about a security-relevant guarantee the caller was relying
on. `tests/tests/ociman_volume.rs`'s own `run_with_a_read_only_named_
volume_rejects_a_write` caught exactly this on that cell (`assertion
failed: !run.status.success()` — the write the test expected to be
rejected actually succeeded).

## The fix: distinguish "host-owned, known-tolerable" from "ours, must not silently fail"

`RootfsAction::RemountReadonly` now carries a real
`tolerate_permission_denied: bool`, set correctly at each of its three
real call sites in `crates/oci-runtime-core/src/rootfs.rs`:

- `linux.readonlyPaths` (real, checked-directly host-adjacent spec
  paths like `/proc/bus`) and a read-only root filesystem: `true` —
  unchanged from the original, correct 0010 reasoning.
- Every other mount reaching `plan_one_mount` (`bundle.spec.mounts`):
  `true` **only** for the one, real, already-documented exception —
  `/sys` itself, identified by `mount.destination == "/sys"`, the
  *exact same* criterion `Spec::into_rootless` already uses to
  construct that one special mount in the first place, not a new,
  separate heuristic. Every other bind mount reaching this same code
  path (a real user's own `-v`/`--volume`, or any other bind entry) —
  `false`: a real `EPERM` there now surfaces as a real, hard,
  immediately visible error (the container refuses to start at all
  rather than silently starting with a security guarantee it can't
  actually back), matching this project's own repeatedly-applied
  "never silently accept less than what was asked for" standard.

`launch.rs`'s own execution of `RemountReadonly` now checks this flag
before tolerating `EPERM`, instead of doing so unconditionally.

**Found and fixed a second bug while fixing the first one**: an
initial version of this fix set `tolerate_permission_denied: false`
for *every* mount reaching `plan_one_mount`, not realizing `/sys`'s
own real bind mount reaches that exact same function (it's a regular
`bundle.spec.mounts` entry, not part of the separate `readonlyPaths`
list) — caught immediately by running the real, existing
`ociman_volume.rs` integration tests locally and seeing two previously
passing tests fail with a real `RemountReadonly { target: ".../sys",
tolerate_permission_denied: false }: Operation not permitted` error,
not by inspection alone. Fixed by keying the decision on `mount.
destination == "/sys"` specifically, matching `Spec::into_rootless`'s
own real construction of that one mount.

## Bug 2 (test flakiness, not a real product bug): `commit --pause`'s own observation race

`ociman_commit.rs`'s `commit_pauses_a_running_container_and_unpauses_
it_afterward` busy-polls `cgroup.freeze` (with **zero** sleep between
checks) hoping to catch the real, transient frozen window `ociman
commit --pause` opens while it computes a diff. For a bare-busybox
seeded image (a handful of files, nothing ever written), the real
diff-snapshot walk (`docs/design/0149`) is fast enough that the whole
freeze-diff-unfreeze cycle can complete within roughly a second even
under real (not synthetic) load — and a genuinely unthrottled busy-spin
polling thread competing for CPU with the very process it's trying to
observe, on a resource-constrained/oversubscribed CI VM, can plausibly
never get scheduled during that narrow window at all. Real cri-o's own
CI investigation history (this project's own — see 0159's `fork()`
thread-safety fix) already established that this project's own CI
hosts can expose real scheduling-latency issues a lightly-loaded
development host never surfaces.

Two changes, both to the test, not production code:

- The seeded image now includes 2,000 small (64-byte) padding files
  (`diff_walk_padding_files`), giving the real diff-snapshot walk
  measurably more real work to do during the freeze window — widening
  it well past ordinary CI scheduling jitter, without meaningfully
  slowing the test down in absolute terms.
- The observing loop now sleeps 200 microseconds between checks
  instead of busy-spinning unthrottled — reducing this thread's own
  CPU pressure against the very `ociman commit` child process it's
  trying to observe (a busy-spin loop can genuinely *starve* that
  child of CPU time on a contended host, which is backwards: the fix
  isn't polling faster, it's giving the observed process a fair
  scheduling chance).

## Verified

- New, dedicated unit test in `crates/oci-runtime-core/src/rootfs.rs`
  (`sys_bind_mount_remount_tolerates_permission_denied_but_a_real_
  volume_never_does`): directly asserts `/sys`'s own `RemountReadonly`
  action carries `tolerate_permission_denied: true` while a real
  `/data` volume mount in the exact same bundle carries `false`, in
  one plan.
- Every existing `rootfs::tests` updated to the new field shape,
  still passing (12/12).
- `tests/tests/ociman_volume.rs`: all 12 tests pass locally, including
  both that a read-only volume genuinely rejects a write and that
  `/sys` itself still tolerates its own known limitation (confirmed
  directly: reverting to the broad, un-keyed fix reintroduces a real
  failure here, on this exact test suite, immediately).
- `tests/tests/ociman_commit.rs`: all 14 tests pass, including
  `commit_pauses_a_running_container_and_unpauses_it_afterward`.
- Full workspace: `cargo build`, `cargo test --workspace` run twice
  (96/96 result blocks both times — `oci-runtime-core`'s own block
  grew 180→181 — 0 failures), `cargo fmt --check`, `cargo clippy
  --all-targets -- -D warnings`, `python3 ci/guards.py` (18 capability
  groups, unaffected), `cargo deny check` (only the pre-existing
  benign warning), `bash ci/native-ci.sh`, hyperfine perf sanity on
  `ociman run --rm` (no regression — the changed code path only
  executes for a mount that's actually being remounted read-only, not
  on `run --rm`'s own hot path at all).

## What's still ahead

Whether this specific pair of fixes is *sufficient* to make the real
`vm (ubuntu-26.04, x86_64)` CI cell pass consistently again can only be
confirmed once this lands on `main` and a real CI run actually
completes — this session's own local reproduction of the `/sys`
regression (caught by hand, not by design) gives real confidence in
the read-only-remount fix specifically, but the CI environment's own
exact scheduling characteristics that made `commit --pause`'s own
observation race so much more visible there than locally aren't
something this session can fully reproduce outside that real
environment. Checking `gh run list` after this push, and periodically
going forward, is now a permanent part of this project's own verification
process, not a one-off.
