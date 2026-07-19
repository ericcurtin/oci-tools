# Design note 0066: `oci-mount` loop device attach/detach (milestone 5)

Status: implemented (attach/detach, read-only, direct-io -- exactly
`oci-mount`'s own long-standing planned scope for this; nothing more)
Scope: `crates/oci-mount/src/loop_device.rs` (new), `crates/oci-mount/
src/lib.rs`, `crates/oci-mount/Cargo.toml`.

`oci-mount`'s own doc comment has named "loop device management
(attach/detach, read-only, direct-io)" as planned scope since its own
first commit. This is the piece that turns a built, sealed erofs image
(0061-0063) into something `mount(2)` can actually target -- the
missing link between "build and seal an image" and "boot from it".

## Direct ioctls, not `losetup` -- matching `oci-erofs::verity`'s own reasoning

`docs/HACKING.md`'s own sanctioned-shellout list does not include
`losetup`. That's consistent with 0062's own reasoning for fs-verity:
loop device attach/detach is a handful of simple ioctls
(`/dev/loop-control`'s `LOOP_CTL_GET_FREE`, a device's own
`LOOP_CONFIGURE`/`LOOP_CLR_FD`) the kernel already exposes directly,
not a complex on-disk format only an external tool can write
(`mkfs.erofs`) or a real cryptographic computation (`veritysetup`).
Unlike fs-verity's own ioctl numbers (`_IOW`/`_IOWR`-encoded, and
`0062` found and fixed a real one-hex-digit typo in exactly that
encoding), `linux/loop.h`'s own `LOOP_*` constants are plain, unencoded
literal values -- lower risk of exactly that mistake recurring, though
still copied and double-checked directly against
`/usr/include/linux/loop.h` on this development host rather than typed
from memory.

## `LOOP_CONFIGURE`, not the older two-step `LOOP_SET_FD`+`LOOP_SET_STATUS64`

`LOOP_CONFIGURE` (Linux 5.8+) sets up and configures a loop device
atomically in one ioctl; this workspace's own two first-class distros
(CentOS Stream 10, Ubuntu 26.04) are both comfortably past that kernel
version, so there's no real reason to implement the older, two-call
dance at all.

## A real, deliberate choice verified by reproducing the wrong behavior first: never set `LO_FLAGS_AUTOCLEAR`

The obvious-looking default would be to let the kernel automatically
tear a loop device down once nothing references it any more
(`LO_FLAGS_AUTOCLEAR`). Tried directly, first: with that flag set, a
device was observed to detach itself the instant the attaching
process exited (confirmed via `losetup -a` no longer listing it
immediately afterward) -- exactly wrong for `ociboot`'s own real use
case, where a deployment's loop device needs to keep existing for as
long as that deployment stays mounted, entirely independent of
whichever short-lived process happened to call `attach`. [`attach`]
deliberately never sets this flag; callers own a device's full
lifecycle explicitly via [`detach`].

## Real, automated tests -- and a real, novel testing problem this workspace hadn't needed before

`/dev/loop-control` and `/dev/loopN` are `root:disk`-owned (confirmed
directly) -- unlike every previous privileged test in this workspace
(`oci-erofs::verity`'s fs-verity tests, `oci-erofs::dmverity`'s
`veritysetup` tests), which only ever needed `sudo` to shell out to an
*external command* for the privileged part while the interesting logic
under test ran unprivileged, [`attach`]/[`detach`] are plain Rust
functions this process itself calls -- there is no CLI to prefix with
`sudo`. Solved with a new pattern for this workspace: each test checks
whether it's already root; if not (the normal case, both on this
development host and the CI VM's own `ci` user), it re-execs the
*same* compiled test binary under `sudo -n`, filtered to just that one
test by its exact name (`sudo -n <exe> --exact <test name>
--nocapture`) -- which runs the identical test function again, this
time genuinely as root, so its real body executes for real; the
original, still-unprivileged invocation just waits for that child and
asserts it succeeded.

Two real, environment-level races were found and fixed while getting
this reliable, both confirmed by direct reproduction, neither a bug in
[`attach`]/[`detach`] themselves:

* `LOOP_CTL_GET_FREE` is not atomic against a second, concurrent
  caller -- running this module's own three tests in parallel (`cargo
  test`'s own default) produced real `EBUSY` failures from two tests
  racing to configure the same just-allocated device number. Fixed
  with a `static Mutex` serializing every test in this module against
  every other one, held across the entire `sudo` re-exec (which blocks
  until the child process finishes either way).
* `LOOP_CLR_FD` can return success while only *scheduling* the clear
  rather than performing it immediately -- a real, documented kernel
  behavior (`drivers/block/loop.c`): if anything else has the device
  node open at the moment of the call, the clear only completes once
  every other opener closes it. Confirmed directly and repeatedly on
  this development host: `systemd-udevd` transiently opens newly
  configured block devices to probe them, occasionally racing a test's
  own immediate "is it really gone now" check. Real `losetup -d` has
  exactly the same kernel-level behavior -- this crate doesn't (and
  shouldn't) paper over it in [`detach`] itself -- but the *test*
  needed to poll briefly (up to 1 second, in 50 ms steps) rather than
  assume synchronous completion the kernel's own API never actually
  promised.

Three tests, each gated behind the real-or-reexeced-root pattern
above: attach then detach a real loop device, cross-checked the whole
way against the real, independent `losetup` binary (attached device's
own backing file is correctly reported; detached device is genuinely
gone, tolerating the scheduling race above); `read_only` genuinely
rejects a real write (`/sys/block/<dev>/ro` reads back `1`, and an
actual write attempt fails with `EPERM`, not just "silently does
nothing"); detaching an already-detached device returns the kernel's
own real `ENXIO`.

Also fixed along the way: a `Path::starts_with` misuse in an early
draft of the first test (`Path::new("/dev/loop23").starts_with("/dev/loop")`
is `false` -- `Path::starts_with` compares whole path *components*,
not a string prefix, a well-known Rust footgun) -- caught immediately
by the test itself failing with a confusing message, fixed by
comparing the parent directory and file name prefix directly instead.

## Performance

This increment adds a brand new module to `oci-mount` and touches no
existing function in it (`options.rs`/`syscalls.rs` are completely
untouched) -- confirmed via `git diff --stat`. `oci-mount` is a real
dependency of `oci-runtime-core` (and therefore rebuilds transitively
whenever its own `Cargo.toml` changes, as this increment's added
`libc`/`tempfile` dependencies do), so a direct `git stash`/`git stash
pop` A/B `hyperfine` comparison was run anyway out of caution
(`ocirun --version`, `ociman run --rm docker.io/library/busybox:latest
-- /bin/true`, 20+ runs each): results were noise-dominated and within
this shared host's already-documented variance, with no plausible
regression mechanism (`loop_device` is new code nothing else calls
yet) -- consistent with no real regression.

## What's still not here

* Anything actually calling [`attach`]/[`detach`] -- `ociboot`'s own
  install/boot flow, which needs this to mount a sealed erofs image at
  all, is a separate, later increment.
* Overlayfs assembly, mount namespaces/propagation control, idmapped
  mounts -- all still exactly as before, `oci-mount`'s own
  longer-standing remaining planned scope.
