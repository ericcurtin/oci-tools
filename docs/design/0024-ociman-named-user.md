# Design note 0024: named `USER` resolution for `ociman run`

Status: implemented
Scope: `bin/ociman/src/user_resolve.rs`, `resolve_user` in `main.rs`.

## The gap

An image's `USER` config field is a string, and real images very
commonly set it to a *name* (`USER root`, `USER app`, `USER nobody`,
...), not a number. `ociman`'s `resolve_user` only ever accepted
`""`/numeric uid/numeric `uid:gid` forms — anything else, including the
extremely common `USER root`, failed outright with "is not numeric".

Note this was never `ocirun`'s (or `crun`'s/`runc`'s) problem: by the
time a runtime-spec reaches a low-level runtime, `process.user` is
already fully numeric per the OCI runtime-spec's own `User` schema.
Name resolution is a higher-level-tool concern done once, before the
spec is synthesized — which is why real `podman` does it in its own
`pkg/lookup` (vendoring `github.com/moby/sys/user`'s `GetExecUser`),
not in `crun`, and why it belongs in `ociman`, not `oci-runtime-core`.

## What `user_resolve::resolve` does

Ported from `moby/sys/user`'s `GetExecUser` (read directly from the
real vendored copy in `~/git/podman/vendor/github.com/moby/sys/user/
user.go` to get the corner cases right), minus supplementary group IDs
— this runtime doesn't support extra gids yet (only ever one uid *and*
one gid is ever mapped into the container's user namespace), so
collecting them would just be dead data with nowhere to go.

Reads `<rootfs>/etc/passwd` and (only if the `USER` string names a
group explicitly) `<rootfs>/etc/group` — the *image's own* files, not
the host's, matching real podman's own `securejoin`-guarded reads (this
increment does a plain read, not `securejoin`'s symlink-escape
protection; noted below as a follow-up, not implemented here).

Rules (matching upstream exactly):
* `""` -> `(0, 0)`.
* A numeric uid is used as-is whether or not it has a passwd entry;
  if it *does*, that entry's own gid becomes the default group (real
  behavior: `USER 0` against a real `/etc/passwd` picks up gid 0 from
  the `root` line, not just a hardcoded default).
* A non-numeric name is only resolvable via a matching passwd entry —
  there's no other way to turn a name into a number, so this is an
  error if there's no `/etc/passwd` at all, or no matching row.
* An explicit `user:group` form resolves the group the same way: a
  numeric group is used as-is, a named one needs an `/etc/group` entry.

`resolve_user` (in `main.rs`) is now just this resolution plus the
already-existing "only container uid 0 is mappable" rejection —
unchanged in spirit from before, just applied to whatever numeric uid
resolution produced instead of only ever a bare numeric string.

## Real, automated tests

`bin/ociman/src/user_resolve.rs`'s own unit tests (10 cases: empty
user, bare numeric with/without a passwd entry, named user via passwd,
unknown name with and without any passwd file present, explicit
numeric/named group overrides, unknown named group, fully-numeric
`uid:gid` needing no files at all).

`tests/tests/ociman_run.rs` gained two full end-to-end cases exercising
the real extraction -> resolution -> launch path:
`run_accepts_a_named_root_user_resolved_via_etc_passwd` (`USER root`
against a seeded `/etc/passwd`, actually running and printing output —
the one non-root-uid-adjacent name that can *fully* succeed today,
since `root` resolves to uid 0) and `run_rejects_a_named_non_root_user`
(`USER app` resolving to uid 1000 via a seeded passwd entry, still
correctly hitting the same "can't map it" rejection a bare numeric
`1000` already did) — proving resolution and the mapping-limitation
check are wired together correctly, not just that `user_resolve` itself
is correct in isolation.

`tests/src/lib.rs`'s `seed_image` gained a `seed_image_with_files`
sibling (extra `(path, contents)` regular files baked into the same
synthetic layer) so these two new tests could seed an `/etc/passwd`
without duplicating `seed_image`'s own tar-building logic; the original
`seed_image` is now `seed_image_with_files` with an empty extra-files
slice, so none of its three existing call sites needed to change.

## Performance

Doesn't touch `oci_runtime_core::launch`/`process`/`identity`/anything
in the fork-to-exec hot path at all — this is pure pre-fork setup in
`ociman`'s own synthesis step (a couple of small file reads before the
runtime spec is even written to disk). No re-benchmark was done for
this increment, consistent with prior increments that only touched
non-hot-path code (0019–0021); the fork/launch path itself is unchanged
from 0023's already-reverified 3.0ms.

## What's still not here

* No `securejoin`-style symlink-escape protection on the `/etc/passwd`/
  `/etc/group` reads — a malicious image whose `/etc/passwd` is itself
  a symlink pointing outside the rootfs could currently cause a read of
  an arbitrary host path (though not a write, and the read result only
  ever feeds a uid/gid decision that's then further restricted by the
  single-uid-mapping check below). Real podman's own `pkg/lookup`
  guards this with `github.com/cyphar/filepath-securejoin`; this
  project doesn't have an equivalent secure-join helper yet.
* No supplementary/additional group IDs (`Sgids` in upstream's own
  type) — this runtime has nowhere to put them yet regardless.
* The single-mapped-uid limitation itself is unchanged: a resolved
  non-root uid, whether it arrived as a number or a name, still can't
  actually run — only *resolution* is new, not *capability*. A
  subordinate uid range via `/etc/subuid` (`newuidmap`/`newgidmap`,
  matching how real rootless podman/buildah handle this) would be
  needed to lift that, and is a substantially bigger, separate change.
