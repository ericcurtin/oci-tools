# Design note 0250: `ociboot-init mount --state-dir` — the writable view

Status: implemented
Scope: `bin/ociboot-init/src/main.rs`, `crates/oci-mount/src/syscalls.rs`,
`crates/oci-mount/src/loop_device.rs` (+`lib.rs` re-exports),
`tests/tests/ociboot_init_mount.rs`.

## The second slice of the boot sequence

0246 landed verify-and-mount; a real OS can't run from a bare
read-only image, though. This slice assembles the writable view the
skeleton's own module docs promised, on top of the still-sealed
erofs, when `--state-dir` (the mounted state partition, in a real
boot) is given:

- **`/etc` — a real overlay**: the image's own `/etc` as `lowerdir`,
  `upperdir`/`workdir` under `<state>/etc/` — so configuration edits
  persist across boots on the state partition while the image stays
  sealed. (ostree solves this with a deploy-time three-way merge;
  this project's own design — "ostree's concepts without ostree" —
  keeps the deltas live on an overlay instead. The *upgrade-time*
  interaction between an upper layer and a new image's `/etc` is
  milestone 6's `/etc` merge, still ahead.)
- **`/var` — bind of `<state>/var`**: all real machine state lives on
  the state partition, byte-for-byte visible on the host side.
- **`/run` + `/tmp` — fresh tmpfs** (`nosuid,nodev`), like every real
  init.
- The image must provide all four directories — a real OS image
  always does; a clear error names whichever is missing rather than a
  bare `ENOENT` from `mount(2)`.

Without `--state-dir`, `mount` stays exactly the 0246 behavior.

## The failure path taught a real lesson

On any writable-view failure the whole target is torn down again —
and the first implementation (one lazy `MNT_DETACH` sweep, then a
loop-device detach) leaked the loop device: the lazy unmount's
completion is *deferred*, so the immediate `LOOP_CLR_FD` raced it,
failed `EBUSY` silently, and the device outlived the "cleanup"
(caught by the integration test's own leak assertions, not by luck).
The teardown now does plain, synchronous unmounts deepest-first
(`tmp`/`run`/`var`/`etc`, then the base) and retries the detach
briefly. `oci-mount` gained the two small shared primitives —
`unmount` (plain) and `unmount_detach` (lazy) — rather than
`ociboot-init` growing its own rustix dependency for them.

That fix uncovered a second, closely related race the same way: the
detach retry loop only re-tried when `LOOP_CLR_FD` itself *errored*,
but the ioctl genuinely can report success while only *scheduling*
the clear — real, documented kernel behavior (`drivers/block/loop.c`)
observed directly and repeatedly on this development host:
`systemd-udevd` transiently opens a just-changed block device to
probe it, and the actual clear only completes once every such opener
closes it again. A loop-safe cleanup path can't tell "genuinely done"
from "merely scheduled" by the ioctl's return code alone — found the
same way as the first race, by a flaky (not deterministic, roughly
40% of runs locally) integration-test failure, not by inspection.
`oci-mount` gained `wait_until_detached` (backed by a direct, real
`LOOP_GET_STATUS64` poll, no shellout) to confirm the real, completed
state; `ociboot-init`'s teardown now retries the *whole*
detach-then-confirm step against its overall deadline rather than
trusting the first successful ioctl call. Verified directly: 10/10
repeated runs of the integration test that used to fail intermittently
now pass, plus two new unit tests in `oci-mount` itself (a real
detach's completion is correctly confirmed; a still-attached device
is never spuriously reported as detached even under a short timeout).

## Verified

Integration, all against real mounts under the sudo gate:

- Overlay lower half readable through the view (`/etc/os-release`
  content from the image); an `/etc` write lands in
  `<state>/etc/upper/` host-visible; a `/var` write lands in
  `<state>/var/`; `/tmp` writes are real but persist nowhere; the
  erofs base still rejects writes *as root*; recursive unmount +
  loop release leave the host clean.
- The failure path end to end: a minimal image (no `/etc`) with
  `--state-dir` errors naming the missing directory, and leaves no
  mount and no loop device behind (the assertions that caught the
  lazy-detach race).
- Full workspace: `cargo build`, `cargo test --workspace`,
  `cargo fmt --all --check`, `cargo clippy --workspace --all-targets
  -- -D warnings`, `python3 ci/guards.py`, `cargo deny check`,
  `bash ci/native-ci.sh`, `ci/build-deb.sh`, `ci/bench.sh` sanity
  (nothing on any container path changed).

## Still ahead

Binding `/ociboot` into the target, mounting the state partition
itself, switch-root, the dm-verity sidecar fallback, karg emission
into BLS entries, and the `90ociboot` dracut module.
