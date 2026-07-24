# Design note 0247: kill-safe verity test fixtures (a real hang + a real leak, fixed)

Status: implemented
Scope: `crates/oci-erofs/src/verity.rs` (test fixture),
`tests/tests/ociboot_build_image.rs` (its replicated sibling),
`tests/Cargo.toml`.

## What actually happened

A routine full-workspace test run hung for its entire 40-minute
ceiling on this project's own dev host, and the post-mortem found
**ten** stale loop devices (plus several live-but-orphaned ext4
loopback mounts) accumulated across sessions, all backed by
`verity.img` files from the fs-verity test fixture — the
`mkfs.ext4 -O verity` loopback mount `oci_erofs::verity`'s own unit
tests (and `ociboot_build_image.rs`'s deliberately-replicated copy)
create under a fresh tempdir per run.

Two distinct defects, both real:

1. **Leak on SIGKILL.** The fixture's cleanup lived in `Drop` — which
   a killed test run (this project's own automation kills overrunning
   commands with SIGKILL; any developer's `kill -9`/OOM does the
   same) never executes. The mount, its loop device, and the tempdir
   all outlived the process, invisibly, forever — per run.
2. **Hang on a sudo prompt.** The fixture's `mount`/`chown`/`umount`
   used plain `sudo` (no `-n`). On a host whose passwordless sudo has
   lapsed (or was never configured), `sudo` blocks on an invisible
   password prompt — a test that *hangs forever* instead of the clean
   skip every other privileged test in this workspace already
   produces via its `sudo -n true` gate. The 40-minute hang has
   exactly this shape.

## The fix: a fixed base + flock-probed staleness

The fixtures now live under a fixed, per-user base
(`/tmp/oci-tools-verity-fixture-<uid>/<pid>-<n>/`), shared by both
copies on purpose. Every live fixture holds an **exclusive flock** on
its own subdirectory's lock file for its whole lifetime; the kernel
releases flocks on process death — *including SIGKILL* — so the next
run's setup sweeps every subdirectory whose lock is acquirable
(a race-free owner-is-dead test: a live, concurrent fixture in
another test process keeps its lock held, making the probe fail —
no timestamp heuristic can offer that guarantee). Sweeping unmounts,
detaches any loop device still attached to the image (autoclear
normally frees it at unmount, but loops outliving their mounts were
part of the observed debris), and removes the directory.

Every `sudo` in these fixtures is now `sudo -n`: a lapsed
passwordless sudo produces a clean skip, never a hang.

## Verified

- Both fixtures' own tests pass leak-free (zero fixture mounts, zero
  loop devices, empty base directory after runs).
- The kill scenario proven end to end: a stale fixture staged exactly
  as a SIGKILLed run leaves it (mounted loop ext4, unheld lock) was
  found, unmounted, detached, and removed by the very next test run's
  sweep — `dirs=0 mounts=0 loops=0` after.
- A repo-wide audit: no remaining plain-`sudo` (without `-n`) in any
  test or fixture.
- The pre-existing host debris (10 loop devices, 9 orphaned mounts)
  cleaned by hand.
- Full workspace: `cargo build`, `cargo test --workspace`,
  `cargo fmt --all --check`, `cargo clippy --workspace --all-targets
  -- -D warnings`, `python3 ci/guards.py`, `cargo deny check`,
  `bash ci/native-ci.sh`, `ci/build-deb.sh` (test-only change; no
  product code touched).
