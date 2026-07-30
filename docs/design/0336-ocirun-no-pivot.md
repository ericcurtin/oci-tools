# Design note 0336: `ocirun run`/`create --no-pivot`

Status: implemented
Scope: `crates/oci-mount/src/{syscalls,lib}.rs`,
`crates/oci-runtime-core/src/launch.rs`, `bin/ocirun/src/main.rs`,
`bin/ocivmm/src/main.rs`, `bin/ocibox/src/main.rs`,
`bin/ociman/src/{main,build}.rs`, `bin/ocicri/src/launcher.rs`,
`tests/tests/ocirun_run.rs`, `tests/tests/ocirun_lifecycle.rs`,
`README.md`.

## What this closes

Real `runc run`/`crun run --no-pivot` (and `create --no-pivot`) had no
equivalent here — flagged repeatedly since `0332` as a real, if
initially-uncertain-in-scope, candidate. A dedicated re-survey (not
just re-reading a prior estimate) confirmed it's genuinely smaller than
first assumed: the core mechanism is entirely built from syscalls this
project already wraps, and a `chroot`-style root swap is purely
additive, opt-in, and confined to a well-isolated code path.

## Real, checked-directly semantics

Read both reference runtimes' own source directly:

- **Real crun** (`~/git/crun/src/libcrun/linux.c`'s own `move_root`,
  called from `libcrun_finalize_mounts` when `no_pivot` is set): `chdir
  (rootfs)` → `umount_or_hide("/sys")` → `umount_or_hide("/proc")` →
  `mount(rootfs, "/", MS_MOVE)` → `chroot(".")` → `chdir("/")`. Its own
  `umount_or_hide` lazily unmounts (`MNT_DETACH`), falling back to
  hiding under an empty (`size=0k`) `tmpfs` if the kernel refuses with
  `EINVAL` (a locked mount).
- **Real runc** (`~/git/runc/libcontainer/rootfs_linux.go`'s own
  `msMoveRoot`) does materially more: it scans `/proc/self/mountinfo`
  for every full host mount under `/proc`/`/sys` and individually
  slave-remounts/masks each one, a real security-hardening step this
  project has no mountinfo parser to implement at all.

The `/sys`/`/proc` unmount-or-hide step is not cosmetic — it closes a
real hazard: unlike `pivot_root(2)` (whose own relocate-then-unmount
step, `RootfsAction::UnmountOldRoot`, fully detaches the *entire* old
mount tree, including its own `/sys`/`/proc`, from the process's mount
namespace), a plain `chroot(2)` never detaches anything at all — the
host's own `/sys`/`/proc` mounts, still reachable via their absolute
paths in the not-yet-chrooted namespace at this exact moment, would
otherwise remain reachable from *inside* the jail afterward too.

## What this project does

Matches real crun's own simpler path exactly, including its own
`/sys`/`/proc` unmount-or-hide step — not the narrower "just 3
syscalls" description an earlier survey pass had assumed before
actually reading crun's source line by line. Deliberately does not
replicate real runc's own broader mountinfo-scanning hardening (a real,
documented divergence, the same "pick the simpler of two reference
implementations, say so" precedent already established elsewhere,
e.g. `docs/design/0327`'s own `Icon=` rewrite quirk). Both reference
runtimes document `--no-pivot` as an escape hatch for exceptional
circumstances only (a nested container with no fresh mount namespace
of its own to `pivot_root` within is the real, narrow case it exists
for) — this project's own narrower implementation is consistent with
that same caution, and the default (`pivot_root`) path is completely
unaffected either way.

## Implementation

New in `oci_mount`: `move_mount(source, target)` (a direct
`rustix::mount::mount_move` wrapper, mirroring `pivot_root`'s own
existing shape) — `unmount_detach`/`mount` already existed and are
reused as-is for the unmount-or-hide fallback.

`ChildSetup` (`launch.rs`) gained `no_pivot: bool` (default `false` in
`build_child_setup`, the same `preserve_fds`-style precedent, `0291`).
`run`, `run_reporting_pid`, and `create` each gained a `no_pivot: bool`
parameter, threaded into `child_setup.no_pivot`. `mount_pivot_and_exec`'s
plan-execution loop special-cases `RootfsAction::PivotRoot`/
`UnmountOldRoot` when `no_pivot` is set: `PivotRoot` diverts to a new
`chroot_style_root_swap(new_root)` (crun's own `move_root`, step for
step, via a new `unmount_or_hide` helper matching crun's own fallback
exactly); `UnmountOldRoot` is simply skipped — there is no relocated
old root to unmount at all on this path, `chroot` never having
relocated anything in the first place.

`Command::Run`/`Command::Create` in `bin/ocirun/src/main.rs` each
gained a real `--no-pivot` flag, threaded through to the two
`ocirun`-only call sites. Every other call site into `launch::run`/
`run_reporting_pid`/`create` (`ocibox`, `ocivmm`, `ociman::main.rs`,
`ociman::build.rs`, `ocicri::launcher.rs` — 5 sites total) gets a
mechanical `false`, each documented with the same one-line "this
binary has no `--no-pivot` flag of its own either" note real
`docker run`/`podman run`/`distrobox enter` etc. also don't expose it.

One real bug found and fixed during implementation, not foreseen in
advance: `chroot_style_root_swap`'s first draft reused `new_root`
verbatim for the `move_mount` source string, after already `chdir`ing
into it — correct only when `new_root` happens to already be an
absolute path (real crun's own C code has this identical shape, but
crun's own caller always resolves the bundle's rootfs to an absolute
path first). A real, relative-bundle-path test case caught this
immediately (`ENOENT`, resolving the relative path a second time
*inside* the directory it had just `chdir`'d into); fixed by using
`"."` for the move-mount source instead, which unambiguously means
"wherever this process just `chdir`'d to" regardless of whether the
original path was relative or absolute.

## Verified

Manual, end-to-end: `ocirun run --no-pivot`/`create --no-pivot`
against a real rootless busybox bundle both produce output
byte-for-byte identical to the default `pivot_root` path (`ls /` shows
only the container's own top level either way), with no leftover
`.oci-tools-put-old` scratch directory on the `--no-pivot` path (there
never was one to clean up in the first place).

Three new integration tests: `run_no_pivot_still_isolates_the_rootfs_
just_like_pivot_root` (`tests/tests/ocirun_run.rs`, 15 total, 12
pre-existing after removing a since-relocated draft) proves real
rootfs isolation and the absence of any pivot-root scratch directory;
`create_no_pivot_reaches_running_after_start`
(`tests/tests/ocirun_lifecycle.rs`, 16 total, 15 pre-existing) proves
`create --no-pivot` reaches a genuinely `running` state after a real
`start`, through `create`'s own separate call site into the identical
shared machinery.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (113 test result blocks,
0 failures), `python3 ci/guards.py`, `cargo deny check`,
`ci/native-ci.sh`, `ci/build-deb.sh` (real `dpkg -i`/`--version`/
`dpkg -r` round trip).

Performance: genuinely hot-startup-path-adjacent code (`launch.rs`'s
own plan-execution loop), so `ci/bench.sh` was re-run in full — every
figure remains consistent with `0331`'s own baseline (e.g. `ocirun run`
vs `crun run` 2.16× here vs 2.26×/2.29× in `0331`/`0314`, `ociman run
--rm` vs `podman run --rm` 5.56× here vs 5.28-5.32× previously), no
regression: the default (non-`--no-pivot`) path only ever pays for one
extra `bool` field check per already-existing loop iteration.

## Still ahead

`ociman`/`ocirun`'s own other remaining gaps (`--restart` policy,
confirmed to need a from-scratch supervisor with zero existing
infrastructure to build on; `--console-socket`, confirmed blocked on
this project's own already-documented "no PTY allocation" gap) and
`ocibox`'s own remaining gaps (`stop`/`upgrade`/`generate-entry`/
`assemble`, `export --sudo`/`--enter-flags`) remain separately-scoped
future candidates.
