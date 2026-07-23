# Design note 0232: a real CI flake fix, a retracted "fix" that wasn't one, and a real fs-verity fallback gap

Status: implemented
Scope: `crates/oci-runtime-core/src/rootfs.rs`; `crates/oci-runtime-core/src/launch.rs`;
`tests/tests/ociman_commit.rs`; `tests/tests/ociman_volume.rs`; `ci/setup-host.sh`;
`crates/oci-erofs/src/verity.rs`; `bin/ociboot/src/main.rs`.

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
exercises remotely, and keep watching subsequent CI runs after each
fix rather than assuming a single green local run means the real
problem is solved.

## Bug 1, corrected: the read-only-remount "fix" was retracted — it broke real, working behavior

This section originally claimed `RemountReadonly`'s unconditional
`EPERM` tolerance was a security bug, and shipped a fix keyed on
`mount.destination == "/sys"` to make every *other* bind+`ro` mount
(including a user's own `-v name:/path:ro` volume) treat `EPERM` as
fatal instead. **That premise was wrong, and the fix has been
reverted** (`crates/oci-runtime-core/src/rootfs.rs`/`launch.rs` are
back to unconditionally tolerating `EPERM` on `RemountReadonly`,
exactly as before this note originally shipped).

What actually happened: the original "fix" happened to make
`tests/tests/ociman_volume.rs`'s `run_with_a_read_only_named_volume_
rejects_a_write` pass (it now failed *before* the container even ran,
so its `!run.status.success()` assertion was trivially satisfied) —
but the very next real CI run showed it had broken two *other*,
pre-existing, legitimate tests on the real `vm (ubuntu-26.04, x86_64)`
cell:

- `ociman_run.rs::run_volume_flag_ro_rejects_a_write_from_inside_the_container`
- `ociman_run.rs::run_volume_flag_bind_mounts_a_real_host_file`

Both failed with `RemountReadonly { ..., tolerate_permission_denied:
false }: Operation not permitted`. Critically, the first of those two
tests' **own doc comment, which predates this session entirely**,
already explained why:

> "A first version of this test asserted a real in-container write
> attempt fails... but it failed inside this project's own VM CI for
> the exact same reason `--read-only`'s own first version did:
> remounting a bind mount read-only can require `CAP_SYS_ADMIN` in the
> namespace that owns the *original* superblock, a real,
> environment-dependent rootless limitation (`docs/design/0010`) this
> project's own `RemountReadonly` handler already tolerates rather
> than treats as fatal."

`docs/design/0010` documented this limitation narrowly (it was only
verified against `/sys` at the time), but the general kernel rule it
describes — `CAP_SYS_ADMIN` in the superblock's *owning* user
namespace is required to remount-readonly a bind mount, which a
fake-root-in-a-userns process does not have for *any* pre-existing
host filesystem, not just `/sys` — applies just as much to an ordinary
`-v host:container:ro` bind mount as it does to `/sys`. A real,
existing test in this codebase (predating this session) had already
generalized the tolerance to volumes for exactly this reason, on
purpose, with its own reasoning recorded. This session's original
"fix" contradicted that established, working design without
recognizing it, based on the mistaken assumption that the broad
tolerance was unintentional scope-creep rather than a deliberate,
previously-verified decision.

**The real, remaining problem** was narrower than originally framed:
`run_with_a_read_only_named_volume_rejects_a_write` was the one test
that hadn't followed the established, safer pattern its own sibling
already used — asserting a *real* in-container write failure (which
depends on whether this specific environment's remount-readonly
actually succeeds) instead of asserting what `ociman` itself can
always control regardless of environment: that it *asked* the kernel
to enforce read-only (the real `config.json` mount recorded a `"ro"`
option). Fixed by rewriting that one test to match its sibling's
pattern — no production code change needed for this part at all.

**Lesson**: a fix that makes one failing test pass isn't verified
until the same fix is checked against every other test touching the
same code path, and — for something this environment-sensitive — until
a subsequent real CI run confirms it, not just a passing local run on
a host that happens not to hit the same kernel limitation (this
project's own aarch64 dev host, unlike the x86_64 CI VM guest,
apparently doesn't hit this particular `CAP_SYS_ADMIN` restriction,
which is exactly why local verification alone never caught either
direction of this mistake).

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

Two changes, both to the test, not production code — this part of the
original fix was correct and is unchanged:

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

## A real, separate CI-infrastructure bug found while checking the fix: `ci/setup-host.sh`

Watching the real CI re-run (per the new "check `gh run list`
afterward" process above) surfaced a second, entirely unrelated,
genuinely deterministic bug: `vm (ubuntu-26.04, x86_64)`/`vm
(centos-stream10, x86_64)` were *also* failing at the "Set up host
(qemu, firmware, kvm)" step, before ever reaching any of this
project's own code. Root cause: `ci/setup-host.sh`'s last statement,
inside a real `/dev/kvm`-present branch, was

```sh
for p in /sys/module/kvm_intel/parameters/unrestricted_guest \
    /sys/module/kvm_amd/parameters/nested; do
    [ -f "$p" ] && echo "setup-host: $p = ..."
done
```

A real x86_64 host has at most one of `kvm_intel`/`kvm_amd` loaded, so
one loop iteration's `[ -f "$p" ]` is always false and `echo` never
runs for it. Since this is the very last command the script ever
executes (no trailing command, no explicit `exit 0`), the *whole
script's own exit status* becomes that final failed test's status: 1
— on every real x86_64 run, completely independent of anything this
project's own code does. Reproduced directly with a minimal script
matching the same structure (`if` branch ending in a `for` loop with
no following command) before trusting the diagnosis. Fixed by using an
explicit `if`/`fi` instead of `&&`, which has exit status 0 when the
condition is false and there's no `else` branch.

## A real, separate application bug found the same way: `ociboot build-image --seal`'s fs-verity fallback

After the `setup-host.sh` fix cleared that step, the next real CI run
showed both x86_64 VM jobs failing for real, inside the test suite
this time: `ociboot_build_image.rs::build_image_seal_falls_back_to_dm_
verity_when_fs_verity_is_unsupported` failed with `sealing
.../deployment.erofs with fs-verity, caused by: Inappropriate ioctl
for device (os error 25)` — `ENOTTY`. `crates/oci-erofs/src/verity.rs`'s
own unit test (`enabling_on_a_non_verity_filesystem_is_unsupported`)
already documented that a filesystem driver that doesn't register
fs-verity operations at all (confirmed there: overlayfs/tmpfs-backed
`/tmp`) returns `ENOTTY`, not `EOPNOTSUPP` — but `bin/ociboot/src/main.
rs`'s own `cmd_build_image` fallback match only checked `io::ErrorKind
::Unsupported` (`EOPNOTSUPP`), so it never learned what the crate's own
test already knew, and hard-failed instead of falling back to
dm-verity on exactly the overlayfs/tmpfs-backed `/tmp` a CI VM guest's
own root filesystem often is.

Reproduced directly, not just inferred from the log: running the
exact failing test locally with `TMPDIR=/dev/shm` (a real tmpfs)
against the pre-fix code reproduces the identical `os error 25`
failure; rebuilding with the fix applied and rerunning passes.

Fixed by extracting the check into one shared function,
`oci_erofs::verity::is_unsupported`, used by both the crate's own unit
test and `cmd_build_image`'s match arm, so they can't drift apart
again.

## Verified

- `crates/oci-runtime-core/src/rootfs.rs`/`launch.rs`: reverted to
  their pre-this-note state (`tolerate_permission_denied` field
  removed again); `rootfs::tests` back to 12/12 passing, no new test
  needed since the reverted code is the already-covered original.
- `tests/tests/ociman_volume.rs`: `run_with_a_read_only_named_volume_
  rejects_a_write` rewritten to check the real, recorded `config.json`
  mount option instead of a real write attempt; all 12 tests pass.
- `tests/tests/ociman_run.rs`: `run_volume_flag_ro_rejects_a_write_
  from_inside_the_container` and `run_volume_flag_bind_mounts_a_real_
  host_file` (the two the retracted fix had broken) both pass again.
- `ci/setup-host.sh`: fixed and confirmed on real CI — both x86_64 VM
  jobs get past "Set up host" cleanly on the very next run.
- `crates/oci-erofs/src/verity.rs`/`bin/ociboot/src/main.rs`: fixed and
  verified locally by reproducing the exact `ENOTTY` failure on real
  tmpfs (`TMPDIR=/dev/shm`) against the old code, then confirming the
  fix resolves it, before ever trusting the CI log alone.
- `tests/tests/ociman_commit.rs`: unchanged from the original version
  of this note, still passing (14/14).
- Full workspace, after all of the above: `cargo build`, `cargo test
  --workspace` run twice (96/96 result blocks both times, 0 failures —
  `oci-runtime-core`'s own block back to 180, matching the reverted
  code), `cargo fmt --check`, `cargo clippy --all-targets -- -D
  warnings`, `python3 ci/guards.py` (18 capability groups, unaffected),
  `cargo deny check` (only the pre-existing benign warning), `bash
  ci/native-ci.sh`, hyperfine perf sanity on `ociman run --rm` (no
  regression).

## What's still ahead

Whether this full set of corrections is sufficient to make the real
`vm (ubuntu-26.04, x86_64)`/`vm (centos-stream10, x86_64)` cells pass
consistently can only be confirmed by watching the next real CI run
after this lands — which is now a permanent, ongoing part of this
project's own process, not a one-off check.
