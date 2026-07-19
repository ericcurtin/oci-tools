# Design note 0031: symlink-escape protection for `/etc/passwd`/`/etc/group` reads

Status: implemented
Scope: `bin/ociman/src/user_resolve.rs`'s `read_optional`.

## The gap

0024 (named `USER` resolution) shipped with a known, explicitly flagged
gap in its own "what's still not here" section: a malicious or corrupt
image whose `/etc/passwd` (or any containing directory) is a symlink
pointing outside the rootfs — e.g. `etc -> /` or
`etc/passwd -> /etc/shadow` — could make `resolve`'s plain
`std::fs::read_to_string` follow it and read an arbitrary *host* file
instead of anything belonging to the image. Real podman's own
`pkg/lookup` guards exactly this with
`github.com/cyphar/filepath-securejoin`.

## The fix: the kernel's own mechanism, not a hand-rolled algorithm

Rather than reimplement `securejoin`'s own component-by-component
symlink-clamping algorithm (subtle to get exactly right, and inherently
race-prone unless every step is also re-verified atomically against
concurrent modification), this uses `openat2(2)`'s `RESOLVE_IN_ROOT`
resolve flag directly (Linux 5.6+, already exposed by `rustix` — no new
dependency at all): it resolves a path against a directory fd *as if*
that fd were `chroot()`ed to, so any symlink encountered along the way
(absolute or relative) and any `..` that would otherwise escape above
it are transparently reinterpreted relative to that same root instead
— atomically, in the kernel, with no TOCTOU window between checking a
path and opening it.

Verified against a real symlink escape attempt before writing any
application code (a scratch program, deleted after): a directory whose
`etc/passwd` was a symlink pointing at a real file elsewhere on the
host produced a plain `ENOENT` from `openat2`/`RESOLVE_IN_ROOT` rather
than ever opening the escape target — confirmed both the escape case
(blocked, `ENOENT`) and the ordinary case (a ordinary in-rootfs file,
opened and read normally) before committing to this approach.

## Scope

This fixes the one place in the current codebase that reads *into* an
extracted rootfs from the host's own mount namespace before the
container itself ever starts (`resolve_user`'s `/etc/passwd`/
`/etc/group` lookups) — the same gap 0024 originally flagged, not a
broader audit of every rootfs interaction. Once a container actually
starts, its own mount namespace has already been `pivot_root`ed onto
the rootfs, so an absolute-looking symlink target *inside* it resolves
within the container's own view of `/` as expected — not a comparable
escape risk.

## Real, automated tests

Two new unit tests in `user_resolve.rs`: a `/etc/passwd` symlink
pointing at a real file *outside* the rootfs is not followed (`resolve`
sees the same "no `/etc/passwd` present" behavior a genuinely missing
file would produce, not a successful read of the escape target); a
`/etc/passwd` symlink whose target stays *inside* the rootfs (an
entirely ordinary thing for a real image to do, e.g. some distros'
usr-merge layouts) still resolves and reads normally — proving
`RESOLVE_IN_ROOT` blocks escapes specifically, not symlinks in general.
All 10 pre-existing `user_resolve` tests continue to pass unchanged.

## Performance

Doesn't touch `oci_runtime_core::launch` or anything in the `run`
fork-to-exec hot path — `resolve_user` runs once per `ociman run`
invocation, before the container even forks, same as before this
change; the only difference is two `openat`-family syscalls per file
read instead of one, an immaterial cost at this scale. No re-benchmark
needed, consistent with prior increments that only touched non-hot-path
code.

## What's still not here

* No equivalent protection anywhere else that might one day read
  host-side into an extracted rootfs before containerization (there is
  no other such place in the codebase today, but a future increment
  adding one should reuse this same `RESOLVE_IN_ROOT` pattern rather
  than a plain `read_to_string`).
* Supplementary group IDs (still the same pre-existing gap 0024 already
  flagged, unrelated to this fix).
